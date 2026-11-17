//! Detection of the TeX toolchains this machine can run.
//!
//! A toolchain is either installed natively (found on `PATH`) or provided by a TeX Live
//! Docker image, so the host needs nothing besides Docker.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;
use ts_rs::TS;

/// TeX Live with every package (scheme-full) but no documentation, rebuilt weekly.
pub const DEFAULT_DOCKER_IMAGE: &str = "texlive/texlive:latest";

/// A TeX toolchain the compile service knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum EngineKind {
    /// `latexmk` from a TeX Live / MiKTeX installation.
    Latexmk,
    /// The self-contained Tectonic engine.
    Tectonic,
}

impl EngineKind {
    pub const ALL: [EngineKind; 2] = [EngineKind::Latexmk, EngineKind::Tectonic];

    /// Name of the executable.
    pub fn binary(self) -> &'static str {
        match self {
            EngineKind::Latexmk => "latexmk",
            EngineKind::Tectonic => "tectonic",
        }
    }

    fn version_args(self) -> &'static [&'static str] {
        match self {
            EngineKind::Latexmk => &["-v"],
            EngineKind::Tectonic => &["--version"],
        }
    }
}

/// Where a toolchain runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "lowercase")]
#[ts(export)]
pub enum EngineRuntime {
    /// An executable installed on this machine.
    Native { path: String },
    /// A throwaway container of this image, without network access.
    Docker { image: String },
}

/// A toolchain that answered its version check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EngineInfo {
    pub kind: EngineKind,
    pub runtime: EngineRuntime,
    /// First non-empty line printed by the version flag, if it answered in time.
    pub version: Option<String>,
}

/// What [`detect_engines`] looks for.
#[derive(Debug, Clone, Default)]
pub struct DetectOptions {
    /// `PATH`-style list searched for executables; `None` uses the process `PATH`.
    pub search_path: Option<OsString>,
    /// TeX Live image whose `latexmk` is run through Docker; `None` skips Docker.
    pub docker_image: Option<String>,
}

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Starting a container can take a while on a cold Docker daemon.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(30);

/// Finds every supported toolchain: native ones first, then the Docker image.
pub async fn detect_engines(options: &DetectOptions) -> Vec<EngineInfo> {
    let search_path = options
        .search_path
        .clone()
        .or_else(|| std::env::var_os("PATH"));
    let search_path = search_path.as_deref();

    let mut found = Vec::new();
    for kind in EngineKind::ALL {
        if let Some(path) = find_binary(kind.binary(), search_path) {
            let version = run(&path, kind.version_args(), VERSION_TIMEOUT)
                .await
                .and_then(|output| first_meaningful_line(&output.stdout));
            found.push(EngineInfo {
                kind,
                runtime: EngineRuntime::Native {
                    path: path.display().to_string(),
                },
                version,
            });
        }
    }
    if let Some(image) = &options.docker_image {
        found.extend(detect_docker(image, search_path).await);
    }
    found
}

async fn detect_docker(image: &str, search_path: Option<&OsStr>) -> Option<EngineInfo> {
    let docker = find_binary("docker", search_path)?;

    // Never pull here: the image is several gigabytes.
    let inspect = run(
        &docker,
        &["image", "inspect", "--format", "{{.Id}}", image],
        DOCKER_TIMEOUT,
    )
    .await?;
    if !inspect.status.success() {
        tracing::warn!(
            image,
            reason = first_meaningful_line(&inspect.stderr).unwrap_or_default(),
            "TeX Live image unavailable; download it with `docker pull {image}`"
        );
        return None;
    }

    let probe = ["run", "--rm", "--network", "none", image, "latexmk", "-v"];
    let Some(output) = run(&docker, &probe, DOCKER_TIMEOUT).await else {
        tracing::warn!(
            image,
            "`latexmk -v` did not finish in the TeX Live container"
        );
        return None;
    };
    if !output.status.success() {
        tracing::warn!(
            image,
            reason = first_meaningful_line(&output.stderr).unwrap_or_default(),
            "cannot run latexmk in the TeX Live container"
        );
        return None;
    }
    Some(EngineInfo {
        kind: EngineKind::Latexmk,
        runtime: EngineRuntime::Docker {
            image: image.to_owned(),
        },
        version: first_meaningful_line(&output.stdout),
    })
}

fn find_binary(name: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    which::which_in(name, search_path, cwd).ok()
}

/// Runs a short command, killing it if it outlives `timeout`.
async fn run(program: &Path, args: &[&str], timeout: Duration) -> Option<Output> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(timeout, output).await.ok()?.ok()
}

fn first_meaningful_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_leading_blank_lines() {
        let latexmk = b"\nLatexmk, John Collins, 7 Jan. 2023. Version 4.79\n";
        assert_eq!(
            first_meaningful_line(latexmk).as_deref(),
            Some("Latexmk, John Collins, 7 Jan. 2023. Version 4.79")
        );
        assert_eq!(first_meaningful_line(b"  \n\n"), None);
    }

    #[cfg(unix)]
    fn fake_bin(dir: &Path, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;

        let bin = dir.join(name);
        std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn options(dir: &Path, docker_image: Option<&str>) -> DetectOptions {
        DetectOptions {
            search_path: Some(dir.as_os_str().to_owned()),
            docker_image: docker_image.map(str::to_owned),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detects_native_tectonic() {
        let dir = tempfile::tempdir().unwrap();
        fake_bin(dir.path(), "tectonic", "echo 'Tectonic 0.17.0'");

        let engines = detect_engines(&options(dir.path(), None)).await;

        assert_eq!(engines.len(), 1);
        assert_eq!(engines[0].kind, EngineKind::Tectonic);
        assert!(matches!(engines[0].runtime, EngineRuntime::Native { .. }));
        assert_eq!(engines[0].version.as_deref(), Some("Tectonic 0.17.0"));
    }

    /// A stand-in for the Docker CLI that records its arguments.
    #[cfg(unix)]
    fn fake_docker(dir: &Path, image_exists: bool) {
        let inspect = if image_exists {
            "echo sha256:0123"
        } else {
            "echo 'Error: No such image: texlive/test' >&2; exit 1"
        };
        let log = dir.join("docker-args");
        fake_bin(
            dir,
            "docker",
            &format!(
                "echo \"$@\" >> '{}'\ncase \"$1\" in\n  image) {inspect} ;;\n  \
                 run) echo 'Latexmk, John Collins, 31 Jan. 2024. Version 4.83' ;;\n  \
                 *) exit 2 ;;\nesac",
                log.display()
            ),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detects_latexmk_in_a_pulled_docker_image() {
        let dir = tempfile::tempdir().unwrap();
        fake_docker(dir.path(), true);

        let engines = detect_engines(&options(dir.path(), Some("texlive/test"))).await;

        assert_eq!(
            engines,
            [EngineInfo {
                kind: EngineKind::Latexmk,
                runtime: EngineRuntime::Docker {
                    image: "texlive/test".into()
                },
                version: Some("Latexmk, John Collins, 31 Jan. 2024. Version 4.83".into()),
            }]
        );
        let calls = std::fs::read_to_string(dir.path().join("docker-args")).unwrap();
        assert!(calls.contains("run --rm --network none texlive/test latexmk -v"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn never_pulls_a_missing_docker_image() {
        let dir = tempfile::tempdir().unwrap();
        fake_docker(dir.path(), false);

        let engines = detect_engines(&options(dir.path(), Some("texlive/test"))).await;

        assert!(engines.is_empty());
        let calls = std::fs::read_to_string(dir.path().join("docker-args")).unwrap();
        assert_eq!(calls.lines().count(), 1, "only `docker image inspect` runs");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skips_docker_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        fake_docker(dir.path(), true);

        assert!(detect_engines(&options(dir.path(), None)).await.is_empty());
        assert!(!dir.path().join("docker-args").exists());
    }

    #[tokio::test]
    async fn empty_path_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let found = detect_engines(&options(dir.path(), Some(DEFAULT_DOCKER_IMAGE))).await;
        assert!(found.is_empty());
    }
}
