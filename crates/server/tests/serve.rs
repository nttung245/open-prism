//! The real `open-prism-api serve --web` binary, with a fake web app (Python standing in for
//! Node) and a fake browser: `--open`, starting again while it runs, a busy port, and stopping
//! when the terminal goes away (SIGHUP).

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

/// The tests start real servers on real ports: one at a time.
static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Answers `--version` like Node 22; otherwise serves `/api/health` like Open Prism on
/// `$HOSTNAME`:`$PORT`, and writes its process id to `$FAKE_WEB_PID` and the address it was
/// given to `$FAKE_WEB_HOST`.
const FAKE_NODE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo v22.0.0; exit 0; fi
exec python3 -c '
import http.server, json, os
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/health":
            body = json.dumps({"version": "0.1.0", "engines": []}).encode()
        else:
            body = b"ok"
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
with open(os.environ["FAKE_WEB_PID"], "w") as f:
    f.write(str(os.getpid()))
with open(os.environ["FAKE_WEB_HOST"], "w") as f:
    f.write(os.environ["HOSTNAME"])
http.server.HTTPServer((os.environ["HOSTNAME"], int(os.environ["PORT"])), Handler).serve_forever()
'
"#;

/// Records the links it is asked to open.
const FAKE_BROWSER: &str = "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$BROWSER_LOG\"\n";

struct Setup {
    dir: tempfile::TempDir,
    node: PathBuf,
    web: PathBuf,
    browser: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let web = dir.path().join("web");
        std::fs::create_dir_all(web.join(".next/static")).unwrap();
        std::fs::write(web.join("server.js"), "").unwrap();
        let node = write_script(dir.path(), "node", FAKE_NODE);
        let browser = write_script(dir.path(), "browser", FAKE_BROWSER);
        Self {
            dir,
            node,
            web,
            browser,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn opened(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("opened"))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn web_host(&self) -> String {
        std::fs::read_to_string(self.path("web.host")).unwrap()
    }

    fn web_pid(&self) -> i32 {
        std::fs::read_to_string(self.path("web.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// `open-prism-api serve --web …` on `ports` (API, web) with `extra` arguments.
    fn command(&self, ports: (u16, u16), extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_open-prism-api"));
        command
            .arg("--data-dir")
            .arg(self.path("data"))
            .args([
                "--port",
                &ports.0.to_string(),
                "--tex-image",
                "none",
                "serve",
            ])
            .arg("--web")
            .arg(&self.web)
            .args(["--web-port", &ports.1.to_string()])
            .arg("--node")
            .arg(&self.node)
            .args(extra)
            .env("BROWSER", &self.browser)
            .env("BROWSER_LOG", self.path("opened"))
            .env("FAKE_WEB_PID", self.path("web.pid"))
            .env("FAKE_WEB_HOST", self.path("web.host"))
            // What a shell or a container may have set: never used as is.
            .env("HOSTNAME", "from-the-shell")
            .env("RUST_LOG", "info")
            .env_remove("OPENPRISM_ACCESS_TOKEN")
            .env_remove("OPENPRISM_OPEN")
            .env_remove("OPENPRISM_WEB_HOST")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    fn start(&self, ports: (u16, u16), extra: &[&str]) -> Running {
        Running::new(self.command(ports, extra).spawn().unwrap())
    }

    fn token(&self) -> String {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_open-prism-api"))
            .arg("--data-dir")
            .arg(self.path("data"))
            .arg("print-token")
            .env_remove("OPENPRISM_ACCESS_TOKEN")
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}

/// Written by a child process, so this multi-threaded test binary never holds the script open
/// for writing while another test forks (exec would fail with "Text file busy").
fn write_script(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    path
}

fn free_ports() -> (u16, u16) {
    let port = || {
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    (port(), port())
}

fn is_free(port: u16) -> bool {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
}

fn is_running(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok()
}

/// A running command and the lines it prints (stdout and stderr together).
struct Running {
    child: Child,
    lines: mpsc::UnboundedReceiver<String>,
    seen: Vec<String>,
}

impl Running {
    fn new(mut child: Child) -> Self {
        let (tx, lines) = mpsc::unbounded_channel();
        fn forward(
            pipe: impl AsyncRead + Unpin + Send + 'static,
            tx: mpsc::UnboundedSender<String>,
        ) {
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = tx.send(line);
                }
            });
        }
        forward(child.stdout.take().unwrap(), tx.clone());
        forward(child.stderr.take().unwrap(), tx);
        Self {
            child,
            lines,
            seen: Vec::new(),
        }
    }

    async fn wait_for(&mut self, needle: &str) {
        let found = tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(line) = self.lines.recv().await {
                self.seen.push(line.clone());
                if line.contains(needle) {
                    return true;
                }
            }
            false
        })
        .await;
        assert!(
            matches!(found, Ok(true)),
            "no line with {needle:?}; output:\n{}",
            self.seen.join("\n")
        );
    }

    async fn exit(&mut self) -> ExitStatus {
        let status = tokio::time::timeout(Duration::from_secs(20), self.child.wait())
            .await
            .expect("still running")
            .unwrap();
        // Drain what is left, for the assertions on the output.
        while let Ok(Some(line)) =
            tokio::time::timeout(Duration::from_millis(200), self.lines.recv()).await
        {
            self.seen.push(line);
        }
        status
    }

    async fn stop(&mut self, signal: Signal) -> ExitStatus {
        let pid = self.child.id().unwrap();
        kill(Pid::from_raw(pid.try_into().unwrap()), signal).unwrap();
        self.exit().await
    }

    fn output(&self) -> String {
        self.seen.join("\n")
    }
}

async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..50 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn open_shows_the_sign_in_link_in_the_browser_once_ready() {
    let _serial = SERIAL.lock().await;
    let setup = Setup::new();
    let ports = free_ports();

    let mut app = setup.start(ports, &["--open"]);
    app.wait_for("Open Prism is running").await;
    let link = format!("http://127.0.0.1:{}/login?token={}", ports.1, setup.token());
    assert!(
        eventually(|| setup.opened() == [link.clone()]).await,
        "opened {:?}, expected {link}",
        setup.opened()
    );
    assert!(app.stop(Signal::SIGTERM).await.success());

    // Without --open, nothing is opened.
    let ports = free_ports();
    let mut quiet = setup.start(ports, &[]);
    quiet.wait_for("Open Prism is running").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(setup.opened().len(), 1);
    assert!(quiet.stop(Signal::SIGTERM).await.success());
}

#[tokio::test]
async fn starting_again_while_running_only_opens_the_browser() {
    let _serial = SERIAL.lock().await;
    let setup = Setup::new();
    let ports = free_ports();
    let mut first = setup.start(ports, &[]);
    first.wait_for("Open Prism is running").await;

    let mut second = setup.start(ports, &["--open"]);
    let status = second.exit().await;
    assert!(status.success(), "{status}: {}", second.output());
    assert!(
        second.output().contains("Open Prism is already running"),
        "{}",
        second.output()
    );
    let link = format!("http://127.0.0.1:{}/login?token={}", ports.1, setup.token());
    assert_eq!(setup.opened(), [link]);

    // The first one is untouched.
    assert!(first.child.try_wait().unwrap().is_none());
    assert!(!is_free(ports.1));
    assert!(first.stop(Signal::SIGTERM).await.success());
}

#[tokio::test]
async fn a_port_taken_by_another_program_is_still_an_error() {
    let _serial = SERIAL.lock().await;
    let setup = Setup::new();
    let ports = free_ports();
    let _other = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, ports.0)).unwrap();

    let mut app = setup.start(ports, &["--open"]);
    let status = app.exit().await;
    assert!(!status.success());
    assert!(
        app.output().contains("already in use") && app.output().contains("--port"),
        "{}",
        app.output()
    );
    assert!(setup.opened().is_empty());
}

#[tokio::test]
async fn a_browser_that_cannot_start_is_only_a_warning() {
    let _serial = SERIAL.lock().await;
    let setup = Setup::new();
    let ports = free_ports();
    let mut command = setup.command(ports, &["--open"]);
    command.env("BROWSER", setup.path("no-such-browser"));
    let mut app = Running::new(command.spawn().unwrap());
    app.wait_for("Open Prism is running").await;
    app.wait_for("cannot open the browser").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(app.child.try_wait().unwrap().is_none(), "{}", app.output());
    assert!(app.stop(Signal::SIGTERM).await.success());
}

#[tokio::test]
async fn closing_the_terminal_stops_the_app_and_the_web_app() {
    let _serial = SERIAL.lock().await;
    let setup = Setup::new();
    let ports = free_ports();
    let mut app = setup.start(ports, &[]);
    app.wait_for("Open Prism is running").await;
    let web = setup.web_pid();
    assert!(is_running(web));

    let status = app.stop(Signal::SIGHUP).await;
    assert!(status.success(), "{status}: {}", app.output());
    assert!(
        eventually(|| !is_running(web)).await,
        "the web app is still running"
    );
    assert!(is_free(ports.0) && is_free(ports.1));
}

#[tokio::test]
async fn the_web_app_listens_on_loopback_unless_told_every_address() {
    let _serial = SERIAL.lock().await;
    let setup = Setup::new();

    let ports = free_ports();
    let mut app = setup.start(ports, &[]);
    app.wait_for("Open Prism is running").await;
    assert_eq!(setup.web_host(), "127.0.0.1");
    assert!(
        !app.output().contains("listens on 0.0.0.0"),
        "{}",
        app.output()
    );
    assert!(app.stop(Signal::SIGTERM).await.success());

    let ports = free_ports();
    let mut app = setup.start(ports, &["--web-host", "0.0.0.0"]);
    app.wait_for("Open Prism is running").await;
    app.wait_for(&format!("http://127.0.0.1:{}/login?token=", ports.1))
        .await;
    assert_eq!(setup.web_host(), "0.0.0.0");
    assert!(
        app.output()
            .contains(&format!("the web app listens on 0.0.0.0:{}", ports.1)),
        "{}",
        app.output()
    );
    assert!(app.stop(Signal::SIGTERM).await.success());

    // Any other address is refused before anything starts.
    let ports = free_ports();
    let mut app = setup.start(ports, &["--web-host", "192.168.1.5"]);
    let status = app.exit().await;
    assert!(!status.success());
    assert!(
        app.output().contains("127.0.0.1") && app.output().contains("0.0.0.0"),
        "{}",
        app.output()
    );
    assert!(is_free(ports.0) && is_free(ports.1));
}
