//! Running a compilation: choosing a toolchain, building the command line, running it in a
//! TeX Live container or natively with cancellation and a timeout, and collecting the output.
//!
//! The project directory and the build directory are mounted into the container at the same
//! absolute paths as on the host, so the paths TeX records (log, SyncTeX) are host paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::engines::{EngineInfo, EngineKind, EngineRuntime};
use crate::paths::RelPath;

/// The TeX engine a project asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ProjectEngine {
    /// Read `% !TEX program = …`, otherwise XeLaTeX when the project needs Unicode fonts or
    /// has Vietnamese text without the T5 encoding, otherwise pdfLaTeX.
    #[default]
    Auto,
    Pdflatex,
    Xelatex,
    Lualatex,
    Tectonic,
}

impl ProjectEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Pdflatex => "pdflatex",
            Self::Xelatex => "xelatex",
            Self::Lualatex => "lualatex",
            Self::Tectonic => "tectonic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "auto" => Self::Auto,
            "pdflatex" => Self::Pdflatex,
            "xelatex" => Self::Xelatex,
            "lualatex" => Self::Lualatex,
            "tectonic" => Self::Tectonic,
            _ => return None,
        })
    }
}

/// The program that actually typeset the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Program {
    Pdflatex,
    Xelatex,
    Lualatex,
    Tectonic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum RuntimeKind {
    Docker,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum CompileStatus {
    /// The engine finished without errors.
    Success,
    /// The engine reported errors (a PDF may still exist).
    Failed,
    /// A newer compilation or the user stopped it.
    Cancelled,
    TimedOut,
    /// The compilation could not start (no engine, bad main file, …).
    Error,
}

/// Packages that only work with a Unicode engine.
const UNICODE_ENGINE_MARKERS: [&str; 5] = [
    "fontspec",
    "unicode-math",
    "polyglossia",
    "xeCJK",
    "\\setmainfont",
];

/// Picks the engine for documents that do not say which one they need. `others` holds the
/// project's other sources (chapters, a shared preamble, its own classes and packages), read
/// for the same signs as the main file except the magic comment.
pub fn detect_program(main: &str, others: &[String]) -> Program {
    for line in main.lines().take(100) {
        let line = line.trim_start();
        if !line.starts_with('%') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if !lower.contains("!tex") || !lower.contains("program") {
            continue;
        }
        if let Some((_, value)) = lower.split_once('=') {
            match value.trim() {
                "xelatex" => return Program::Xelatex,
                "lualatex" | "lualatexmk" => return Program::Lualatex,
                "pdflatex" | "latex" => return Program::Pdflatex,
                _ => {}
            }
        }
    }
    let code: Vec<String> = std::iter::once(main)
        .chain(others.iter().map(String::as_str))
        .map(without_comments)
        .collect();
    let needs_unicode_engine = code.iter().any(|code| {
        UNICODE_ENGINE_MARKERS
            .iter()
            .any(|marker| code.contains(marker))
    });
    // pdfLaTeX drops Vietnamese letters (with an error for each) unless the T5 encoding is
    // loaded; XeLaTeX's default font has them all.
    let vietnamese_without_t5 = code.iter().any(|code| has_vietnamese_letters(code))
        && !code.iter().any(|code| sets_up_vietnamese(code));
    if needs_unicode_engine || vietnamese_without_t5 {
        Program::Xelatex
    } else {
        Program::Pdflatex
    }
}

fn without_comments(source: &str) -> String {
    source
        .lines()
        .map(strip_comment)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Letters only Vietnamese uses: ơ, ư, and the letters with two diacritics or a dot below
/// (ạ, ả, ấ … ỹ, the Latin Extended Additional block from U+1EA0).
fn has_vietnamese_letters(code: &str) -> bool {
    code.chars()
        .any(|c| matches!(c, 'Ơ' | 'ơ' | 'Ư' | 'ư' | '\u{1EA0}'..='\u{1EF9}'))
}

/// Whether the code sets up pdfLaTeX for Vietnamese: babel's `vietnamese` option (also as
/// a class option), the `vietnam` or `vntex` package, or the T5 font encoding.
fn sets_up_vietnamese(code: &str) -> bool {
    code.lines().any(|line| {
        let declares = ["\\usepackage", "\\RequirePackage", "\\documentclass"]
            .iter()
            .any(|command| line.contains(command));
        declares
            && (line.contains("vietnam")
                || line.contains("vntex")
                || (line.contains("fontenc") && line.contains("T5")))
    })
}

/// Most files besides the main one read by [`other_sources`], and their largest size.
const MAX_OTHER_SOURCES: usize = 200;
const MAX_OTHER_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

/// Extensions of the files [`other_sources`] reads: a class or package of the project is part
/// of its preamble, like a `.tex` file it inputs (thesis templates set up fonts in their class).
const SOURCE_EXTENSIONS: [&str; 3] = [".tex", ".cls", ".sty"];

/// The project's other `.tex` files, and its own classes and packages, for [`detect_program`].
fn other_sources(root: &Path, main: &RelPath) -> Vec<String> {
    let Ok(entries) = crate::files::list_tree(root) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|entry| {
            entry.kind == crate::files::EntryKind::File
                && entry.size <= MAX_OTHER_SOURCE_BYTES
                && entry.path != main.as_str()
                && {
                    let path = entry.path.to_ascii_lowercase();
                    SOURCE_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
                }
        })
        .take(MAX_OTHER_SOURCES)
        .filter_map(|entry| std::fs::read(root.join(&entry.path)).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect()
}

/// Recreates the folders of the project in the output directory. pdfLaTeX writes the `.aux`
/// file of `\include{chapters/intro}` to `<outdir>/chapters/intro.aux` and fails when that
/// folder is missing; latexmk then makes one missing folder per run and gives up after a few
/// ("needed too many passes"), so a thesis split into folders failed its first compile.
fn make_project_dirs(project_root: &Path, build_dir: &Path) -> std::io::Result<()> {
    // A project too big to list just keeps latexmk's slower way.
    let Ok(entries) = crate::files::list_tree(project_root) else {
        return Ok(());
    };
    for entry in entries {
        if entry.kind != crate::files::EntryKind::Dir {
            continue;
        }
        if let Ok(folder) = RelPath::parse(&entry.path) {
            std::fs::create_dir_all(folder.to_path(build_dir))?;
        }
    }
    Ok(())
}

/// The part of a line before an unescaped `%`.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'%' && (index == 0 || bytes[index - 1] != b'\\') {
            return &line[..index];
        }
    }
    line
}

/// Everything needed to compile one project once.
#[derive(Debug, Clone)]
pub struct CompileJob {
    /// Absolute path of the project directory (the TeX working directory).
    pub project_root: PathBuf,
    pub main_file: RelPath,
    pub engine: ProjectEngine,
    /// Absolute, persistent output directory (`-outdir`), reused for incremental builds.
    pub build_dir: PathBuf,
    /// Where the finished outputs of this run are copied (`output.pdf`, …).
    pub run_dir: PathBuf,
    /// Persistent cache mounted into the container (`TEXMFVAR`, `TMPDIR`).
    pub tex_cache_dir: PathBuf,
    /// Name of the container, so it can be killed.
    pub container_name: String,
    pub timeout: Duration,
}

/// Result of running the toolchain, before log analysis.
#[derive(Debug, Clone)]
pub struct CompileOutcome {
    pub status: CompileStatus,
    pub program: Option<Program>,
    pub runtime: Option<RuntimeKind>,
    pub duration: Duration,
    pub exit_code: Option<i32>,
    /// Whether `run_dir/output.pdf` was written by this run.
    pub has_pdf: bool,
    /// The TeX `.log`, if any.
    pub log: Option<String>,
    /// The biber/BibTeX `.blg`, if any.
    pub bib_log: Option<BibLog>,
    /// Console output of latexmk/tectonic (last part).
    pub console: String,
    /// Why the compilation could not run, for [`CompileStatus::Error`].
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BibLog {
    Biber(String),
    Bibtex(String),
}

/// Output files of a run inside `run_dir`.
pub const PDF_FILE: &str = "output.pdf";
pub const SYNCTEX_FILE: &str = "output.synctex.gz";
pub const LOG_FILE: &str = "output.log";
pub const CONSOLE_FILE: &str = "console.txt";

/// Console output kept per run.
const CONSOLE_LIMIT: usize = 64 * 1024;
/// Grace period for the engine to exit after it was asked to stop.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Runs `job` with the best available toolchain and collects its outputs into `job.run_dir`.
pub async fn compile(
    job: &CompileJob,
    engines: &[EngineInfo],
    cancel: &CancellationToken,
) -> CompileOutcome {
    let started = Instant::now();
    let started_at = SystemTime::now();
    let failure = |message: String| CompileOutcome {
        status: CompileStatus::Error,
        program: None,
        runtime: None,
        duration: started.elapsed(),
        exit_code: None,
        has_pdf: false,
        log: None,
        bib_log: None,
        console: String::new(),
        message: Some(message),
    };

    let main_path = job.main_file.to_path(&job.project_root);
    let source = match tokio::fs::read(&main_path).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) => {
            return failure(format!(
                "cannot read the main file `{}`: {err}",
                job.main_file
            ));
        }
    };
    let others = if job.engine == ProjectEngine::Auto {
        let root = job.project_root.clone();
        let main = job.main_file.clone();
        tokio::task::spawn_blocking(move || other_sources(&root, &main))
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let (program, engine) = match choose_toolchain(job.engine, &source, &others, engines) {
        Ok(choice) => choice,
        Err(message) => return failure(message),
    };
    for dir in [
        &job.build_dir,
        &job.run_dir,
        &job.tex_cache_dir.join("texmf-var"),
        &job.tex_cache_dir.join("tmp"),
    ] {
        if let Err(err) = tokio::fs::create_dir_all(dir).await {
            return failure(format!("cannot create {}: {err}", dir.display()));
        }
    }
    let (root, build_dir) = (job.project_root.clone(), job.build_dir.clone());
    let folders = tokio::task::spawn_blocking(move || make_project_dirs(&root, &build_dir))
        .await
        .unwrap_or(Ok(()));
    if let Err(err) = folders {
        return failure(format!(
            "cannot create the project's folders in {}: {err}",
            job.build_dir.display()
        ));
    }
    let invocation = match build_invocation(job, program, engine) {
        Ok(invocation) => invocation,
        Err(message) => return failure(message),
    };

    let execution = execute(&invocation, cancel, job.timeout).await;
    let runtime = Some(invocation.runtime);
    let console = execution.console;
    let _ = tokio::fs::write(job.run_dir.join(CONSOLE_FILE), &console).await;

    let (status, exit_code) = match execution.exit {
        Exit::SpawnFailed(message) => {
            let mut outcome = failure(message);
            outcome.program = Some(program);
            outcome.runtime = runtime;
            return outcome;
        }
        Exit::Cancelled => (CompileStatus::Cancelled, None),
        Exit::TimedOut => (CompileStatus::TimedOut, None),
        Exit::Code(Some(0)) => (CompileStatus::Success, Some(0)),
        Exit::Code(code) => (CompileStatus::Failed, code),
    };

    let stem = job.main_file.file_stem();
    let output = |ext: &str| job.build_dir.join(format!("{stem}.{ext}"));
    // An unchanged document leaves the PDF untouched, so an old PDF counts after a clean exit.
    let pdf_is_current = is_fresh(&output("pdf"), started_at).await || exit_code == Some(0);
    let mut has_pdf = false;
    if matches!(status, CompileStatus::Success | CompileStatus::Failed) && pdf_is_current {
        has_pdf = tokio::fs::copy(output("pdf"), job.run_dir.join(PDF_FILE))
            .await
            .is_ok();
        let _ = tokio::fs::copy(output("synctex.gz"), job.run_dir.join(SYNCTEX_FILE)).await;
    }
    let log = read_lossy(&output("log")).await;
    if log.is_some() {
        let _ = tokio::fs::copy(output("log"), job.run_dir.join(LOG_FILE)).await;
    }
    let bib_log = read_lossy(&output("blg")).await.map(|text| {
        if text.contains("This is BibTeX") {
            BibLog::Bibtex(text)
        } else {
            BibLog::Biber(text)
        }
    });

    CompileOutcome {
        status,
        program: Some(program),
        runtime,
        duration: started.elapsed(),
        exit_code,
        has_pdf,
        log,
        bib_log,
        console,
        message: None,
    }
}

async fn is_fresh(path: &Path, since: SystemTime) -> bool {
    tokio::fs::metadata(path)
        .await
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| modified >= since)
}

async fn read_lossy(path: &Path) -> Option<String> {
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Picks the program and the toolchain that runs it. Docker is preferred over a native
/// installation because it isolates the document from the host. `other_sources` are the
/// project's other sources, read by [`detect_program`] for `Auto`.
pub fn choose_toolchain<'a>(
    requested: ProjectEngine,
    main_source: &str,
    other_sources: &[String],
    engines: &'a [EngineInfo],
) -> Result<(Program, &'a EngineInfo), String> {
    let latexmk = engines
        .iter()
        .filter(|engine| engine.kind == EngineKind::Latexmk)
        .min_by_key(|engine| match engine.runtime {
            EngineRuntime::Docker { .. } => 0,
            EngineRuntime::Native { .. } => 1,
        });
    let tectonic = engines
        .iter()
        .find(|engine| engine.kind == EngineKind::Tectonic);

    let program = match requested {
        ProjectEngine::Tectonic => {
            return tectonic
                .map(|engine| (Program::Tectonic, engine))
                .ok_or_else(|| "Tectonic is not installed on this machine".to_owned());
        }
        ProjectEngine::Auto => detect_program(main_source, other_sources),
        ProjectEngine::Pdflatex => Program::Pdflatex,
        ProjectEngine::Xelatex => Program::Xelatex,
        ProjectEngine::Lualatex => Program::Lualatex,
    };
    match (latexmk, tectonic) {
        (Some(engine), _) => Ok((program, engine)),
        (None, Some(engine)) if requested == ProjectEngine::Auto => Ok((Program::Tectonic, engine)),
        _ => Err(
            "no TeX engine available: pull the TeX Live image (`just tex-pull`) or \
                  install latexmk"
                .to_owned(),
        ),
    }
}

/// A command line ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub runtime: RuntimeKind,
    /// `docker` binary and container name, to stop a Docker run.
    pub container: Option<(PathBuf, String)>,
}

/// Keeps TeX logs parseable: no line wrapping at 79 characters.
const TEX_ENV: [(&str, &str); 5] = [
    ("max_print_line", "10000"),
    ("error_line", "254"),
    ("half_error_line", "238"),
    ("openout_any", "p"),
    ("openin_any", "p"),
];

pub fn build_invocation(
    job: &CompileJob,
    program: Program,
    engine: &EngineInfo,
) -> Result<Invocation, String> {
    let main = job.main_file.as_str();
    let mut outdir = OsString::from("-outdir=");
    outdir.push(&job.build_dir);
    let mut latexmk_args: Vec<OsString> = vec![
        match program {
            Program::Xelatex => "-xelatex",
            Program::Lualatex => "-lualatex",
            Program::Pdflatex | Program::Tectonic => "-pdf",
        }
        .into(),
        "-f".into(),
        "-interaction=nonstopmode".into(),
        "-file-line-error".into(),
        "-synctex=1".into(),
        outdir,
    ];

    match (&engine.runtime, program) {
        (EngineRuntime::Native { path }, Program::Tectonic) => {
            let mut args: Vec<OsString> = ["-X", "compile", "--synctex", "--keep-logs"]
                .map(OsString::from)
                .to_vec();
            args.extend(["--outdir".into(), job.build_dir.clone().into_os_string()]);
            args.push(main.into());
            Ok(Invocation {
                program: PathBuf::from(path),
                args,
                env: Vec::new(),
                cwd: job.project_root.clone(),
                runtime: RuntimeKind::Native,
                container: None,
            })
        }
        (EngineRuntime::Native { path }, _) => {
            // A project's latexmkrc is Perl code: never run it on the host.
            latexmk_args.push("-norc".into());
            latexmk_args.push(main.into());
            Ok(Invocation {
                program: PathBuf::from(path),
                args: latexmk_args,
                env: TEX_ENV
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                cwd: job.project_root.clone(),
                runtime: RuntimeKind::Native,
                container: None,
            })
        }
        (EngineRuntime::Docker { image }, _) => {
            let docker = which::which("docker")
                .map_err(|_| "the `docker` command is not available".to_owned())?;
            let cache = &job.tex_cache_dir;
            let mut args: Vec<OsString> = [
                "run",
                "--rm",
                "--name",
                &job.container_name,
                "--network",
                "none",
                "--read-only",
                "--tmpfs",
                "/tmp:exec,size=512m",
                "--memory",
                "2g",
                "--pids-limit",
                "256",
            ]
            .map(OsString::from)
            .to_vec();
            if let Some(user) = owner_of(&job.project_root) {
                args.extend(["--user".into(), user.into()]);
            }
            for (key, value) in [
                ("HOME", "/tmp"),
                ("TEXMFVAR", "/cache/texmf-var"),
                ("TMPDIR", "/cache/tmp"),
            ]
            .into_iter()
            .chain(TEX_ENV)
            {
                args.extend(["-e".into(), format!("{key}={value}").into()]);
            }
            for (source, target) in [
                (&job.project_root, &job.project_root),
                (&job.build_dir, &job.build_dir),
                (cache, &PathBuf::from("/cache")),
            ] {
                args.extend(["--mount".into(), bind_mount(source, target)?]);
            }
            args.extend(["-w".into(), job.project_root.clone().into_os_string()]);
            args.extend([image.into(), "latexmk".into()]);
            args.extend(latexmk_args);
            args.push(main.into());
            Ok(Invocation {
                program: docker.clone(),
                args,
                env: Vec::new(),
                cwd: job.project_root.clone(),
                runtime: RuntimeKind::Docker,
                container: Some((docker, job.container_name.clone())),
            })
        }
    }
}

fn bind_mount(source: &Path, target: &Path) -> Result<OsString, String> {
    let (Some(source), Some(target)) = (source.to_str(), target.to_str()) else {
        return Err("data directory paths must be valid UTF-8 to use Docker".to_owned());
    };
    // `--mount` takes comma-separated key=value pairs.
    if source.contains([',', '"']) || target.contains([',', '"']) {
        return Err(format!(
            "`{source}` contains a comma or quote, which Docker mounts cannot express"
        ));
    }
    Ok(format!("type=bind,source={source},target={target}").into())
}

/// `uid:gid` of the directory's owner, so files written by the container belong to them.
#[cfg(unix)]
fn owner_of(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some(format!("{}:{}", meta.uid(), meta.gid()))
}

#[cfg(not(unix))]
fn owner_of(_path: &Path) -> Option<String> {
    None
}

enum Exit {
    Code(Option<i32>),
    Cancelled,
    TimedOut,
    SpawnFailed(String),
}

struct Execution {
    exit: Exit,
    console: String,
}

async fn execute(
    invocation: &Invocation,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Execution {
    if let Some((docker, name)) = &invocation.container {
        // A container left over from a crashed server would block the name.
        let _ = run_quiet(docker, &["rm", "-f", name]).await;
    }

    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .envs(invocation.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&invocation.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    if invocation.container.is_none() {
        // Own process group, so the engines latexmk starts can be stopped with it.
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return Execution {
                exit: Exit::SpawnFailed(format!(
                    "cannot start {}: {err}",
                    invocation.program.display()
                )),
                console: String::new(),
            };
        }
    };
    let stdout = child.stdout.take().map(|pipe| tokio::spawn(read_all(pipe)));
    let stderr = child.stderr.take().map(|pipe| tokio::spawn(read_all(pipe)));

    enum Wake {
        Exited(Option<i32>),
        Cancelled,
        TimedOut,
    }
    let wake = tokio::select! {
        status = child.wait() => Wake::Exited(status.ok().and_then(|s| s.code())),
        () = cancel.cancelled() => Wake::Cancelled,
        () = tokio::time::sleep(timeout) => Wake::TimedOut,
    };
    let exit = match wake {
        Wake::Exited(code) => Exit::Code(code),
        Wake::Cancelled => {
            stop(invocation, &mut child).await;
            Exit::Cancelled
        }
        Wake::TimedOut => {
            stop(invocation, &mut child).await;
            Exit::TimedOut
        }
    };

    let mut console = Vec::new();
    for task in [stdout, stderr].into_iter().flatten() {
        if let Ok(bytes) = task.await {
            console.extend(bytes);
        }
    }
    let start = console.len().saturating_sub(CONSOLE_LIMIT);
    Execution {
        exit,
        console: String::from_utf8_lossy(&console[start..]).into_owned(),
    }
}

async fn read_all(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes).await;
    bytes
}

/// Stops a running compilation: `docker kill` for containers (killing the CLI would leave the
/// container running), the whole process group for native runs.
async fn stop(invocation: &Invocation, child: &mut Child) {
    if let Some((docker, name)) = &invocation.container {
        let _ = run_quiet(docker, &["kill", name]).await;
    } else {
        #[cfg(unix)]
        signal_group(child, nix::sys::signal::Signal::SIGTERM);
    }
    if tokio::time::timeout(STOP_GRACE, child.wait())
        .await
        .is_err()
    {
        #[cfg(unix)]
        signal_group(child, nix::sys::signal::Signal::SIGKILL);
        let _ = child.kill().await;
    }
}

#[cfg(unix)]
fn signal_group(child: &Child, signal: nix::sys::signal::Signal) {
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), signal);
    }
}

async fn run_quiet(program: &Path, args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    tokio::time::timeout(Duration::from_secs(20), status)
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("timed out")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(kind: EngineKind, path: &str) -> EngineInfo {
        EngineInfo {
            kind,
            runtime: EngineRuntime::Native { path: path.into() },
            version: None,
        }
    }

    fn docker() -> EngineInfo {
        EngineInfo {
            kind: EngineKind::Latexmk,
            runtime: EngineRuntime::Docker {
                image: "texlive/texlive:latest".into(),
            },
            version: None,
        }
    }

    fn detect(main: &str) -> Program {
        detect_program(main, &[])
    }

    #[test]
    fn detects_the_program_from_magic_comments_and_packages() {
        assert_eq!(detect("\\documentclass{article}"), Program::Pdflatex);
        assert_eq!(
            detect("% !TEX program = lualatex\n\\documentclass{article}"),
            Program::Lualatex
        );
        assert_eq!(detect("% !TeX TS-program = XeLaTeX\n"), Program::Xelatex);
        assert_eq!(detect("\\usepackage{amsmath,fontspec}"), Program::Xelatex);
        assert_eq!(
            detect("% \\usepackage{fontspec}\n\\documentclass{article}"),
            Program::Pdflatex
        );
        assert_eq!(detect("50\\% fontspec-free"), Program::Xelatex);
        // A shared preamble in another file counts too.
        assert_eq!(
            detect_program("\\input{preamble}", &["\\usepackage{fontspec}".to_owned()]),
            Program::Xelatex
        );
    }

    #[test]
    fn vietnamese_text_without_t5_needs_xelatex() {
        // The article template with a Vietnamese project name: pdfLaTeX drops ử, ạ, ộ, ệ.
        let article = "\\documentclass{article}\n\\usepackage{amsmath}\n\\title{Thử chạy một lệnh}\n\\begin{document}\n\\maketitle\n\\end{document}\n";
        assert_eq!(detect(article), Program::Xelatex);
        assert_eq!(detect("Cơ sở dữ liệu"), Program::Xelatex);
        assert_eq!(detect("Như vậy"), Program::Xelatex);

        // pdfLaTeX set up for Vietnamese: keep it.
        for setup in [
            "\\usepackage[T5]{fontenc}",
            "\\usepackage[T1,T5]{fontenc}",
            "\\usepackage[vietnamese]{babel}",
            "\\usepackage[english,vietnamese]{babel}",
            "\\documentclass[12pt,vietnamese]{report}",
            "\\usepackage[utf8]{vietnam}",
            "\\usepackage{vntex}",
        ] {
            assert_eq!(
                detect(&format!("{setup}\n{article}")),
                Program::Pdflatex,
                "{setup}"
            );
        }
        // …also from a shared preamble file.
        assert_eq!(
            detect_program(article, &["\\usepackage[T5]{fontenc}".to_owned()]),
            Program::Pdflatex
        );

        // Vietnamese only in a chapter.
        let main =
            "\\documentclass{report}\n\\begin{document}\n\\input{chapters/intro}\n\\end{document}";
        assert_eq!(detect(main), Program::Pdflatex);
        assert_eq!(
            detect_program(main, &["\\chapter{Mở đầu}".to_owned()]),
            Program::Xelatex
        );

        // Comments, other languages, and the word "vietnam" in the text change nothing.
        assert_eq!(detect("% Ghi chú: sửa lại\nHello"), Program::Pdflatex);
        assert_eq!(detect("Café, naïve, Đorđe, Ærøskøbing"), Program::Pdflatex);
        assert_eq!(
            detect("\\url{https://vietnamnet.vn}\nBài viết"),
            Program::Xelatex
        );
        // A magic comment wins.
        assert_eq!(
            detect(&format!("% !TEX program = pdflatex\n{article}")),
            Program::Pdflatex
        );
    }

    #[test]
    fn other_sources_are_the_other_tex_files_classes_and_packages() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("chapters")).unwrap();
        std::fs::create_dir_all(root.join("styles")).unwrap();
        std::fs::write(root.join("main.tex"), "main").unwrap();
        std::fs::write(root.join("chapters/intro.tex"), "Mở đầu").unwrap();
        std::fs::write(root.join("chapters/NOTES.TEX"), "notes").unwrap();
        std::fs::write(root.join("thesis.cls"), "class").unwrap();
        std::fs::write(root.join("styles/macros.sty"), "package").unwrap();
        std::fs::write(root.join("refs.bib"), "@book{x}").unwrap();
        std::fs::write(root.join("figure.png"), [0x89, 0x50]).unwrap();

        let mut found = other_sources(root, &RelPath::parse("main.tex").unwrap());
        found.sort();
        assert_eq!(found, ["Mở đầu", "class", "notes", "package"]);
        assert!(
            other_sources(&root.join("missing"), &RelPath::parse("main.tex").unwrap()).is_empty()
        );
    }

    #[test]
    fn the_output_directory_gets_the_folders_of_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let build = dir.path().join("build");
        std::fs::create_dir_all(root.join("chapters/front")).unwrap();
        std::fs::create_dir_all(root.join("chapters/main")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("chapters/main/one.tex"), "one").unwrap();
        std::fs::write(root.join("main.tex"), "main").unwrap();
        std::fs::create_dir_all(&build).unwrap();

        make_project_dirs(&root, &build).unwrap();

        for folder in ["chapters", "chapters/front", "chapters/main", "empty"] {
            assert!(build.join(folder).is_dir(), "{folder}");
        }
        // Only folders: no file is copied.
        assert!(!build.join("main.tex").exists());
        assert!(!build.join("chapters/main/one.tex").exists());
        // A second run, with the folders already there, is fine.
        make_project_dirs(&root, &build).unwrap();
    }

    #[test]
    fn a_class_of_the_project_that_sets_up_vietnamese_keeps_pdflatex() {
        // Thesis templates load T5 in their own class, not in main.tex.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let main = "\\documentclass{hcmuitthesis}\n\\title{Khóa luận tốt nghiệp}\n";
        std::fs::write(root.join("main.tex"), main).unwrap();
        std::fs::write(
            root.join("hcmuitthesis.cls"),
            "\\LoadClass{memoir}\n\\RequirePackage[T5]{fontenc}\n\\RequirePackage[utf8]{vietnam}\n",
        )
        .unwrap();

        let others = other_sources(root, &RelPath::parse("main.tex").unwrap());
        assert_eq!(detect_program(main, &others), Program::Pdflatex);
    }

    #[test]
    fn prefers_docker_and_falls_back() {
        let engines = [native(EngineKind::Latexmk, "/usr/bin/latexmk"), docker()];
        let (program, engine) = choose_toolchain(ProjectEngine::Auto, "", &[], &engines).unwrap();
        assert_eq!(program, Program::Pdflatex);
        assert!(matches!(engine.runtime, EngineRuntime::Docker { .. }));
        let chapter = ["Mở đầu".to_owned()];
        let (program, _) = choose_toolchain(ProjectEngine::Auto, "", &chapter, &engines).unwrap();
        assert_eq!(program, Program::Xelatex);
        // An engine chosen by hand is kept.
        let (program, _) =
            choose_toolchain(ProjectEngine::Pdflatex, "", &chapter, &engines).unwrap();
        assert_eq!(program, Program::Pdflatex);

        let only_tectonic = [native(EngineKind::Tectonic, "/usr/bin/tectonic")];
        let (program, _) = choose_toolchain(ProjectEngine::Auto, "", &[], &only_tectonic).unwrap();
        assert_eq!(program, Program::Tectonic);
        assert!(choose_toolchain(ProjectEngine::Xelatex, "", &[], &only_tectonic).is_err());
        assert!(choose_toolchain(ProjectEngine::Tectonic, "", &[], &engines).is_err());
        assert!(choose_toolchain(ProjectEngine::Auto, "", &[], &[]).is_err());
    }

    fn job(root: &Path) -> CompileJob {
        CompileJob {
            project_root: root.join("projects/p1"),
            main_file: RelPath::parse("main.tex").unwrap(),
            engine: ProjectEngine::Auto,
            build_dir: root.join("build/p1"),
            run_dir: root.join("build/p1/runs/r1"),
            tex_cache_dir: root.join("cache/tex"),
            container_name: "openprism-compile-p1".into(),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn native_latexmk_never_reads_rc_files() {
        let job = job(Path::new("/data"));
        let invocation = build_invocation(
            &job,
            Program::Xelatex,
            &native(EngineKind::Latexmk, "/usr/bin/latexmk"),
        )
        .unwrap();
        let args: Vec<_> = invocation
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-xelatex",
                "-f",
                "-interaction=nonstopmode",
                "-file-line-error",
                "-synctex=1",
                "-outdir=/data/build/p1",
                "-norc",
                "main.tex"
            ]
        );
        assert!(
            invocation
                .env
                .contains(&("max_print_line".into(), "10000".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn docker_runs_isolated_with_same_path_mounts() {
        if which::which("docker").is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let job = job(dir.path());
        std::fs::create_dir_all(&job.project_root).unwrap();
        let invocation = build_invocation(&job, Program::Pdflatex, &docker()).unwrap();
        let line = invocation
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        let root = job.project_root.display();
        for expected in [
            "run --rm --name openprism-compile-p1 --network none --read-only".to_owned(),
            format!("--mount type=bind,source={root},target={root}"),
            "-e TEXMFVAR=/cache/texmf-var".to_owned(),
            format!("-w {root} texlive/texlive:latest latexmk -pdf"),
        ] {
            assert!(line.contains(&expected), "missing `{expected}` in `{line}`");
        }
        assert!(line.contains("--user "));
        assert!(!line.contains("-norc"));
    }

    #[test]
    fn rejects_paths_docker_cannot_mount() {
        assert!(bind_mount(Path::new("/a,b"), Path::new("/x")).is_err());
        assert!(bind_mount(Path::new("/a b"), Path::new("/a b")).is_ok());
    }

    /// A fake latexmk that writes a PDF, a log and SyncTeX data into `-outdir`.
    #[cfg(unix)]
    fn fake_latexmk(dir: &Path, script_body: &str) -> EngineInfo {
        let path = dir.join("latexmk");
        crate::test_support::write_script(
            &path,
            &format!(
                "for a in \"$@\"; do case \"$a\" in -outdir=*) out=\"${{a#-outdir=}}\";; esac; done\n{script_body}"
            ),
        );
        native(EngineKind::Latexmk, path.to_str().unwrap())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn collects_outputs_of_a_native_run() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_latexmk(
            dir.path(),
            "echo '%PDF-1.5' > \"$out/main.pdf\"; echo 'This is pdfTeX' > \"$out/main.log\"; \
             echo 'This is BibTeX' > \"$out/main.blg\"; echo console-line; exit 0",
        );
        let job = job(dir.path());
        std::fs::create_dir_all(&job.project_root).unwrap();
        std::fs::write(
            job.project_root.join("main.tex"),
            "\\documentclass{article}",
        )
        .unwrap();

        let outcome = compile(&job, &[engine], &CancellationToken::new()).await;

        assert_eq!(outcome.status, CompileStatus::Success, "{outcome:?}");
        assert_eq!(outcome.program, Some(Program::Pdflatex));
        assert!(outcome.has_pdf);
        assert!(job.run_dir.join(PDF_FILE).is_file());
        assert_eq!(outcome.log.as_deref(), Some("This is pdfTeX\n"));
        assert!(matches!(outcome.bib_log, Some(BibLog::Bibtex(_))));
        assert!(outcome.console.contains("console-line"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_pdf_is_ignored_after_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fake_latexmk(dir.path(), "exit 12");
        let job = job(dir.path());
        std::fs::create_dir_all(&job.project_root).unwrap();
        std::fs::create_dir_all(&job.build_dir).unwrap();
        std::fs::write(job.project_root.join("main.tex"), "x").unwrap();
        std::fs::write(job.build_dir.join("main.pdf"), "old").unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(job.build_dir.join("main.pdf"))
            .unwrap()
            .set_modified(old)
            .unwrap();

        let outcome = compile(&job, &[engine], &CancellationToken::new()).await;

        assert_eq!(outcome.status, CompileStatus::Failed);
        assert_eq!(outcome.exit_code, Some(12));
        assert!(!outcome.has_pdf);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_and_timeout_stop_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-survived");
        // The child process would create the marker if it outlived the stop.
        let engine = fake_latexmk(
            dir.path(),
            &format!("(sleep 1; touch '{}') & sleep 30", marker.display()),
        );
        let mut job = job(dir.path());
        std::fs::create_dir_all(&job.project_root).unwrap();
        std::fs::write(job.project_root.join("main.tex"), "x").unwrap();

        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            trigger.cancel();
        });
        let started = Instant::now();
        let outcome = compile(&job, std::slice::from_ref(&engine), &cancel).await;
        assert_eq!(outcome.status, CompileStatus::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));

        job.timeout = Duration::from_millis(300);
        let outcome = compile(&job, &[engine], &CancellationToken::new()).await;
        assert_eq!(outcome.status, CompileStatus::TimedOut);

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "a child of latexmk kept running");
    }

    /// Real compilations in the TeX Live container: `cargo test -- --ignored`.
    async fn docker_engine() -> EngineInfo {
        let engines = crate::engines::detect_engines(&crate::engines::DetectOptions {
            search_path: None,
            docker_image: Some(crate::engines::DEFAULT_DOCKER_IMAGE.to_owned()),
        })
        .await;
        engines
            .into_iter()
            .find(|e| matches!(e.runtime, EngineRuntime::Docker { .. }))
            .expect("Docker with the texlive/texlive image is required")
    }

    fn docker_job(dir: &Path, name: &str, source: &str) -> CompileJob {
        let root = dir.canonicalize().unwrap();
        let mut job = job(&root);
        job.container_name = format!("openprism-test-{name}-{}", std::process::id());
        std::fs::create_dir_all(&job.project_root).unwrap();
        std::fs::write(job.project_root.join("main.tex"), source).unwrap();
        job
    }

    #[tokio::test]
    #[ignore = "needs Docker and the texlive/texlive image"]
    async fn compiles_in_docker() {
        let engine = docker_engine().await;
        let dir = tempfile::tempdir().unwrap();
        let job = docker_job(
            dir.path(),
            "ok",
            "\\documentclass{article}\n\\begin{document}\nHello \\LaTeX.\n\\end{document}\n",
        );

        let outcome = compile(&job, &[engine], &CancellationToken::new()).await;

        assert_eq!(
            outcome.status,
            CompileStatus::Success,
            "{}",
            outcome.console
        );
        assert_eq!(outcome.runtime, Some(RuntimeKind::Docker));
        assert!(outcome.has_pdf);
        assert!(job.run_dir.join(SYNCTEX_FILE).is_file());
        assert!(outcome.log.unwrap().contains("This is pdfTeX"));
    }

    #[tokio::test]
    #[ignore = "needs Docker and the texlive/texlive image"]
    async fn compiles_vietnamese_in_docker() {
        let engine = docker_engine().await;
        let dir = tempfile::tempdir().unwrap();
        // The article template with a Vietnamese project name, and a Vietnamese chapter.
        let mut job = docker_job(
            dir.path(),
            "vi",
            "\\documentclass{article}\n\\usepackage{amsmath}\n\\usepackage{hyperref}\n\
             \\title{Thử chạy một lệnh}\n\\begin{document}\n\\maketitle\n\
             \\input{chapter}\n\\end{document}\n",
        );
        // XeLaTeX builds its font cache on a first run.
        job.timeout = Duration::from_secs(120);
        std::fs::write(
            job.project_root.join("chapter.tex"),
            "\\section{Giới thiệu}\nĐặng Ửng: ơ ư ạ ả ấ ầ ẩ ẫ ậ ỹ.\n",
        )
        .unwrap();

        let outcome = compile(&job, &[engine], &CancellationToken::new()).await;

        assert_eq!(
            outcome.status,
            CompileStatus::Success,
            "{}",
            outcome.console
        );
        assert_eq!(outcome.program, Some(Program::Xelatex));
        let log = outcome.log.unwrap();
        assert!(log.contains("This is XeTeX"));
        assert!(!log.contains("Missing character"), "{log}");
    }

    #[tokio::test]
    #[ignore = "needs Docker and the texlive/texlive image"]
    async fn includes_from_many_folders_compile_on_the_first_run() {
        let engine = docker_engine().await;
        let dir = tempfile::tempdir().unwrap();
        // Each `\include` in a folder of its own: pdfLaTeX writes its .aux file there.
        let folders = [
            "front",
            "chapters/one",
            "chapters/two",
            "chapters/three",
            "back",
        ];
        let includes: String = folders
            .iter()
            .map(|folder| format!("\\include{{{folder}/part}}\n"))
            .collect();
        let mut job = docker_job(
            dir.path(),
            "include",
            &format!(
                "\\documentclass{{report}}\n\\begin{{document}}\n\\tableofcontents\n\
                 {includes}\\end{{document}}\n"
            ),
        );
        for (index, folder) in folders.iter().enumerate() {
            let path = job.project_root.join(folder);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("part.tex"),
                format!("\\chapter{{Part {index}}}\\label{{part{index}}}\nSee page \\pageref{{part0}}.\n"),
            )
            .unwrap();
        }
        // Several pdfLaTeX runs.
        job.timeout = Duration::from_secs(60);

        let outcome = compile(&job, &[engine], &CancellationToken::new()).await;

        assert_eq!(
            outcome.status,
            CompileStatus::Success,
            "{}",
            outcome.console
        );
        assert!(outcome.has_pdf);
    }

    #[tokio::test]
    #[ignore = "needs Docker and the texlive/texlive image"]
    async fn cancelling_a_docker_run_removes_the_container() {
        let engine = docker_engine().await;
        let dir = tempfile::tempdir().unwrap();
        // An endless loop: TeX never finishes on its own.
        let job = docker_job(
            dir.path(),
            "loop",
            "\\documentclass{article}\n\\begin{document}\n\\def\\x{\\x}\\x\n\\end{document}\n",
        );
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            trigger.cancel();
        });

        let outcome = compile(&job, &[engine], &cancel).await;
        assert_eq!(outcome.status, CompileStatus::Cancelled);

        let listed = std::process::Command::new("docker")
            .args([
                "ps",
                "-aq",
                "--filter",
                &format!("name={}", job.container_name),
            ])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
            "the container is still there"
        );
    }

    #[tokio::test]
    async fn missing_main_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let job = job(dir.path());
        let outcome = compile(&job, &[docker()], &CancellationToken::new()).await;
        assert_eq!(outcome.status, CompileStatus::Error);
        assert!(outcome.message.unwrap().contains("main.tex"));
    }
}
