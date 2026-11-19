//! The web app (the standalone Next.js server) that `serve --web` runs next to the API: checks
//! made before starting it, then a supervisor that waits until it listens, restarts it when it
//! dies, and stops it on shutdown.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::auth::TOKEN_ENV;

/// The oldest Node.js major version the web app supports.
pub const MIN_NODE_MAJOR: u32 = 22;
const NODE_VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_POLL: Duration = Duration::from_millis(100);
const RESTART_DELAY: Duration = Duration::from_secs(1);
const STOP_GRACE: Duration = Duration::from_secs(5);
/// The web app giving up: this many exits within `EXIT_PERIOD`.
const MAX_EXITS: usize = 3;
const EXIT_PERIOD: Duration = Duration::from_secs(60);

/// The major version in `node --version` output such as `v22.11.0`.
pub fn parse_node_version(output: &str) -> Option<u32> {
    let major = output.trim().strip_prefix('v')?.split('.').next()?;
    if major.is_empty() || !major.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    major.parse().ok()
}

/// Checks that `node` is Node.js 22 or newer; returns its major version.
pub async fn check_node(node: &Path) -> anyhow::Result<u32> {
    let name = node.display();
    let advice = format!(
        "the web app needs Node.js {MIN_NODE_MAJOR} or newer: install it, or pass its path with --node"
    );
    let output = Command::new(node)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(NODE_VERSION_TIMEOUT, output).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => bail!("cannot run {name} ({err}); {advice}"),
        Err(_) => bail!("`{name} --version` did not answer within 5 s; {advice}"),
    };
    let printed = String::from_utf8_lossy(&output.stdout);
    let printed = printed.trim();
    match parse_node_version(printed) {
        Some(major) if output.status.success() && major >= MIN_NODE_MAJOR => Ok(major),
        Some(_) if output.status.success() => bail!("{name} is Node.js {printed}; {advice}"),
        _ => bail!("{name} does not look like Node.js (`--version` printed {printed:?}); {advice}"),
    }
}

/// Checks that `dir` is a standalone build of the web app, scripts and styles included;
/// returns it as an absolute path.
pub fn check_web_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let name = dir.display();
    let build = "build it with `just build`, then pass --web web/.next/standalone";
    if !dir.join("server.js").is_file() {
        bail!("{name} is not a standalone build of the web app (no server.js): {build}");
    }
    if !dir.join(".next").join("static").is_dir() {
        bail!(
            "{name} has no .next/static (the web app's scripts and styles): {build}, which copies them"
        );
    }
    std::path::absolute(dir).with_context(|| format!("cannot resolve {name}"))
}

/// Checks that nothing listens on 127.0.0.1:`port` yet.
pub fn check_web_port(port: u16) -> anyhow::Result<()> {
    if port == 0 {
        bail!("--web-port must be a fixed port, not 0");
    }
    match std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => bail!(
            "port {port} on 127.0.0.1 is already in use (is another Open Prism still running?): \
             stop that program, or pass another port with --web-port"
        ),
        Err(err) => bail!("cannot listen on 127.0.0.1:{port}: {err}"),
    }
}

/// The recent exits of the web app, to stop restarting it when it keeps dying.
#[derive(Debug, Default)]
pub struct RestartWindow {
    exits: VecDeque<Instant>,
}

impl RestartWindow {
    /// Records an exit at `now`. `false` when it is the third exit within a minute.
    pub fn record(&mut self, now: Instant) -> bool {
        while self
            .exits
            .front()
            .is_some_and(|first| now.saturating_duration_since(*first) >= EXIT_PERIOD)
        {
            self.exits.pop_front();
        }
        self.exits.push_back(now);
        self.exits.len() < MAX_EXITS
    }
}

/// A web server process to supervise.
#[derive(Debug, Clone)]
pub struct WebServer {
    program: PathBuf,
    args: Vec<OsString>,
    dir: PathBuf,
    env: Vec<(&'static str, String)>,
    port: u16,
    ready_timeout: Duration,
    restart_delay: Duration,
    stop_grace: Duration,
}

impl WebServer {
    /// The standalone Next.js server in `dir`, run with `node` on 127.0.0.1:`port`, forwarding
    /// `/api/*` to the API on 127.0.0.1:`api_port`.
    pub fn next(node: PathBuf, dir: PathBuf, port: u16, api_port: u16) -> Self {
        Self {
            program: node,
            args: vec!["server.js".into()],
            dir,
            env: vec![
                ("PORT", port.to_string()),
                // Overrides a HOSTNAME set by the shell: never listen beyond loopback.
                ("HOSTNAME", Ipv4Addr::LOCALHOST.to_string()),
                ("OPENPRISM_API_URL", format!("http://127.0.0.1:{api_port}")),
                ("NODE_ENV", "production".to_owned()),
                ("NEXT_TELEMETRY_DISABLED", "1".to_owned()),
            ],
            port,
            ready_timeout: READY_TIMEOUT,
            restart_delay: RESTART_DELAY,
            stop_grace: STOP_GRACE,
        }
    }

    /// Where the web app listens.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.dir)
            // The web app has no use for the token: the API checks the session cookie.
            .env_remove(TOKEN_ENV)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // A group of its own: Ctrl+C in the terminal reaches only us, and we stop the web app.
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        command
    }

    fn spawn(&self) -> anyhow::Result<Child> {
        let mut child = self
            .command()
            .spawn()
            .with_context(|| format!("cannot start the web app with {}", self.program.display()))?;
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(forward_lines(stdout, false));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(forward_lines(stderr, true));
        }
        Ok(child)
    }

    /// Runs the web app until `cancel`: sends on `ready` once it listens, restarts it when it
    /// dies, and gives up (an error) when it dies before it is ready or keeps dying.
    pub async fn supervise(
        self,
        cancel: CancellationToken,
        ready: oneshot::Sender<()>,
    ) -> anyhow::Result<()> {
        let mut child = self.spawn()?;
        let started = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            result = self.wait_ready(&mut child) => Some(result),
        };
        match started {
            None => {
                stop(&mut child, self.stop_grace).await;
                return Ok(());
            }
            Some(Err(err)) => {
                stop(&mut child, self.stop_grace).await;
                return Err(err);
            }
            Some(Ok(())) => {}
        }
        let _ = ready.send(());

        let mut exits = RestartWindow::default();
        loop {
            let exited = tokio::select! {
                biased;
                () = cancel.cancelled() => None,
                status = child.wait() => Some(status),
            };
            let Some(status) = exited else {
                stop(&mut child, self.stop_grace).await;
                return Ok(());
            };
            let status = status.context("cannot wait for the web app")?;
            if !exits.record(Instant::now()) {
                bail!(
                    "the web app stopped {MAX_EXITS} times within a minute (last: {status}); \
                     see its log above"
                );
            }
            tracing::warn!(%status, "the web app stopped; restarting it");
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                () = tokio::time::sleep(self.restart_delay) => {}
            }
            child = self.spawn()?;
        }
    }

    /// Waits until something listens on the web app's port.
    async fn wait_ready(&self, child: &mut Child) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + self.ready_timeout;
        loop {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, self.port))
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::select! {
                status = child.wait() => {
                    let status = status.context("cannot wait for the web app")?;
                    bail!("the web app exited before it was ready ({status}); see its log above");
                }
                () = tokio::time::sleep_until(deadline) => bail!(
                    "the web app did not listen on 127.0.0.1:{} within {} s",
                    self.port,
                    self.ready_timeout.as_secs()
                ),
                () = tokio::time::sleep(READY_POLL) => {}
            }
        }
    }
}

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// The link that signs in to the web app at `url` with `token`.
pub fn sign_in_link(url: &str, token: &str) -> String {
    use std::fmt::Write;
    // A token set by hand may hold characters that mean something in a URL.
    let mut encoded = String::with_capacity(token.len());
    for byte in token.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    format!("{url}/login?token={encoded}")
}

/// Logs each line the web app prints, as coming from `web`; stderr lines as warnings.
async fn forward_lines(pipe: impl AsyncRead + Unpin, stderr: bool) {
    let mut reader = BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        if text.trim().is_empty() {
            continue;
        }
        if stderr {
            tracing::warn!(target: "web", "{text}");
        } else {
            tracing::info!(target: "web", "{text}");
        }
    }
}

/// Asks the web app to stop (its whole process group on Unix), then kills it after `grace`.
async fn stop(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    signal_group(child, nix::sys::signal::Signal::SIGTERM);
    #[cfg(not(unix))]
    let _ = child.start_kill();
    if tokio::time::timeout(grace, child.wait()).await.is_err() {
        #[cfg(unix)]
        signal_group(child, nix::sys::signal::Signal::SIGKILL);
        let _ = child.kill().await;
    }
}

#[cfg(unix)]
fn signal_group(child: &Child, signal: nix::sys::signal::Signal) {
    // `id()` is `None` once the child has been reaped, so a reused PID is never signalled.
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), signal);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn node_versions() {
        assert_eq!(parse_node_version("v22.11.0\n"), Some(22));
        assert_eq!(parse_node_version("v24.1.0"), Some(24));
        assert_eq!(parse_node_version("v20.11.0"), Some(20));
        assert_eq!(parse_node_version(""), None);
        assert_eq!(parse_node_version("22.1.0"), None);
        assert_eq!(parse_node_version("vx.1"), None);
        assert_eq!(parse_node_version("Python 3.12.3"), None);
    }

    #[tokio::test]
    async fn a_missing_node_names_the_fix() {
        let err = check_node(Path::new("/nonexistent/node"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Node.js 22 or newer"), "{err}");
        assert!(err.contains("--node"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_old_node_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("node");
        std::fs::write(&fake, "#!/bin/sh\necho v20.11.0\n").unwrap();
        let mut permissions = std::fs::metadata(&fake).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&fake, permissions).unwrap();
        let err = check_fake_node(&fake).await.unwrap_err().to_string();
        assert!(err.contains("is Node.js v20.11.0"), "{err}");
        assert!(err.contains("Node.js 22 or newer"), "{err}");

        std::fs::write(&fake, "#!/bin/sh\necho v22.11.0\n").unwrap();
        assert_eq!(check_fake_node(&fake).await.unwrap(), 22);
    }

    /// `check_node` on a script just written: another test forking at that moment can hold
    /// the file open for writing a little longer ("Text file busy"), so try again.
    #[cfg(unix)]
    async fn check_fake_node(fake: &Path) -> anyhow::Result<u32> {
        for _ in 0..20 {
            match check_node(fake).await {
                Err(err) if err.to_string().contains("Text file busy") => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                result => return result,
            }
        }
        check_node(fake).await
    }

    #[test]
    fn web_dir_needs_server_js_and_static_files() {
        let dir = tempfile::tempdir().unwrap();
        let err = check_web_dir(dir.path()).unwrap_err().to_string();
        assert!(err.contains("no server.js"), "{err}");
        assert!(err.contains("just build"), "{err}");

        std::fs::write(dir.path().join("server.js"), "").unwrap();
        let err = check_web_dir(dir.path()).unwrap_err().to_string();
        assert!(err.contains("no .next/static"), "{err}");
        assert!(err.contains("just build"), "{err}");

        std::fs::create_dir_all(dir.path().join(".next/static")).unwrap();
        let resolved = check_web_dir(dir.path()).unwrap();
        assert!(resolved.is_absolute());
    }

    #[test]
    fn a_busy_web_port_names_the_fix() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let err = check_web_port(port).unwrap_err().to_string();
        assert!(err.contains(&format!("port {port}")), "{err}");
        assert!(err.contains("--web-port"), "{err}");
        drop(listener);
        check_web_port(port).unwrap();
        assert!(check_web_port(0).is_err());
    }

    #[test]
    fn sign_in_links_escape_the_token() {
        let token = "0123456789abcdef".repeat(4);
        assert_eq!(
            sign_in_link("http://127.0.0.1:3737", &token),
            format!("http://127.0.0.1:3737/login?token={token}")
        );
        assert_eq!(
            sign_in_link("http://127.0.0.1:3737", "a b&c#d/é"),
            "http://127.0.0.1:3737/login?token=a%20b%26c%23d%2F%C3%A9"
        );
    }

    #[test]
    fn restarts_stop_at_the_third_exit_within_a_minute() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);

        let mut window = RestartWindow::default();
        assert!(window.record(at(0)));
        assert!(window.record(at(30)));
        assert!(!window.record(at(59)));

        // Spread over more than a minute: the oldest exit no longer counts.
        let mut window = RestartWindow::default();
        assert!(window.record(at(0)));
        assert!(window.record(at(30)));
        assert!(window.record(at(61)));
        assert!(!window.record(at(62)));
    }

    #[test]
    fn the_web_app_gets_its_settings_but_never_the_token() {
        let web = WebServer::next("node".into(), "/srv/web".into(), 3799, 8799);
        let command = web.command();
        let envs: Vec<_> = command.as_std().get_envs().collect();
        assert!(envs.contains(&(OsStr::new(TOKEN_ENV), None)));
        for (key, value) in [
            ("PORT", "3799"),
            ("HOSTNAME", "127.0.0.1"),
            ("OPENPRISM_API_URL", "http://127.0.0.1:8799"),
            ("NODE_ENV", "production"),
        ] {
            assert!(
                envs.contains(&(OsStr::new(key), Some(OsStr::new(value)))),
                "{key}"
            );
        }
        assert_eq!(command.as_std().get_program(), "node");
        assert_eq!(
            command.as_std().get_current_dir(),
            Some(Path::new("/srv/web"))
        );
        assert_eq!(web.url(), "http://127.0.0.1:3799");
    }

    #[cfg(unix)]
    mod process {
        use super::*;

        /// A fake web app: `sh -c script`, ready when something listens on `port`.
        fn fake(port: u16, script: &str) -> WebServer {
            WebServer {
                program: "sh".into(),
                args: vec!["-c".into(), script.into()],
                dir: std::env::temp_dir(),
                env: vec![("PORT", port.to_string())],
                port,
                ready_timeout: Duration::from_secs(5),
                restart_delay: Duration::from_millis(10),
                stop_grace: Duration::from_millis(300),
            }
        }

        fn free_port() -> u16 {
            let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.local_addr().unwrap().port()
        }

        /// Pretends the web app listens, so the supervisor sees it ready at once.
        fn listening() -> (std::net::TcpListener, u16) {
            let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            (listener, port)
        }

        async fn read_pid(path: &Path) -> i32 {
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(path)
                    && let Ok(pid) = text.trim().parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("the fake web app did not write its PID");
        }

        fn is_running(pid: i32) -> bool {
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
        }

        #[tokio::test]
        async fn dying_before_ready_is_an_error() {
            let (ready, readied) = oneshot::channel();
            let err = fake(free_port(), "exit 3")
                .supervise(CancellationToken::new(), ready)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("exited before it was ready"), "{err}");
            assert!(readied.await.is_err());
        }

        #[tokio::test]
        async fn not_listening_in_time_is_an_error() {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let mut web = fake(
                free_port(),
                &format!("echo $$ > {}; exec sleep 60", pid_file.display()),
            );
            web.ready_timeout = Duration::from_millis(500);
            let (ready, _readied) = oneshot::channel();
            let err = web
                .supervise(CancellationToken::new(), ready)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("did not listen"), "{err}");
            assert!(!is_running(read_pid(&pid_file).await));
        }

        #[tokio::test]
        async fn a_web_app_that_keeps_dying_is_given_up() {
            let (_listener, port) = listening();
            let dir = tempfile::tempdir().unwrap();
            let runs = dir.path().join("runs");
            let script = format!("echo run >> {}; sleep 0.2; exit 1", runs.display());
            let (ready, readied) = oneshot::channel();
            let task = tokio::spawn(fake(port, &script).supervise(CancellationToken::new(), ready));
            readied.await.unwrap();
            let err = tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .to_string();
            assert!(err.contains("3 times within a minute"), "{err}");
            // Started once, then restarted twice.
            assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 3);
        }

        #[tokio::test]
        async fn a_web_app_killed_once_is_restarted() {
            let (_listener, port) = listening();
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let script = format!(
                "rm -f {pid}; echo $$ > {pid}; exec sleep 60",
                pid = pid_file.display()
            );
            let cancel = CancellationToken::new();
            let (ready, readied) = oneshot::channel();
            let task = tokio::spawn(fake(port, &script).supervise(cancel.clone(), ready));
            readied.await.unwrap();
            let first = read_pid(&pid_file).await;
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(first),
                nix::sys::signal::Signal::SIGKILL,
            )
            .unwrap();
            let mut second = first;
            for _ in 0..100 {
                second = read_pid(&pid_file).await;
                if second != first {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_ne!(second, first, "the web app was not restarted");
            assert!(is_running(second));
            cancel.cancel();
            task.await.unwrap().unwrap();
            assert!(!is_running(second));
        }

        #[tokio::test]
        async fn cancel_stops_the_web_app_without_restarting_it() {
            let (_listener, port) = listening();
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let script = format!("echo $$ > {}; exec sleep 60", pid_file.display());
            let cancel = CancellationToken::new();
            let (ready, readied) = oneshot::channel();
            let task = tokio::spawn(fake(port, &script).supervise(cancel.clone(), ready));
            readied.await.unwrap();
            let pid = read_pid(&pid_file).await;

            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(6), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(!is_running(pid));
        }

        #[tokio::test]
        async fn a_web_app_ignoring_sigterm_is_killed() {
            let (_listener, port) = listening();
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let script = format!(
                "trap '' TERM; echo $$ > {}; while :; do sleep 0.05; done",
                pid_file.display()
            );
            let cancel = CancellationToken::new();
            let (ready, readied) = oneshot::channel();
            let task = tokio::spawn(fake(port, &script).supervise(cancel.clone(), ready));
            readied.await.unwrap();
            let pid = read_pid(&pid_file).await;

            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(6), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(!is_running(pid));
        }

        #[tokio::test]
        async fn the_web_app_receives_its_settings() {
            let (_listener, port) = listening();
            let dir = tempfile::tempdir().unwrap();
            let env_file = dir.path().join("env");
            let script = format!(
                "env > {}.tmp; mv {0}.tmp {0}; exec sleep 60",
                env_file.display()
            );
            let cancel = CancellationToken::new();
            let (ready, readied) = oneshot::channel();
            let task = tokio::spawn(fake(port, &script).supervise(cancel.clone(), ready));
            readied.await.unwrap();
            let mut env = String::new();
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(&env_file) {
                    env = text;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            cancel.cancel();
            task.await.unwrap().unwrap();
            assert!(
                env.lines().any(|line| line == format!("PORT={port}")),
                "{env}"
            );
            assert!(!env.contains(TOKEN_ENV), "{env}");
        }
    }
}
