//! Detection of the TeX toolchains installed on this machine.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;
use ts_rs::TS;

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

    /// Name of the executable looked up on `PATH`.
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

/// An installed toolchain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EngineInfo {
    pub kind: EngineKind,
    pub path: String,
    /// First non-empty line printed by the version flag, if it answered in time.
    pub version: Option<String>,
}

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Finds every supported toolchain on the process `PATH`.
pub async fn detect_engines() -> Vec<EngineInfo> {
    let search_path = std::env::var_os("PATH");
    detect_engines_in(search_path.as_deref().map(Path::new)).await
}

/// Same as [`detect_engines`] but searches the given `PATH`-style list instead.
pub async fn detect_engines_in(search_path: Option<&Path>) -> Vec<EngineInfo> {
    let mut found = Vec::new();
    for kind in EngineKind::ALL {
        if let Some(path) = find_binary(kind, search_path) {
            let version = read_version(&path, kind.version_args()).await;
            found.push(EngineInfo {
                kind,
                path: path.display().to_string(),
                version,
            });
        }
    }
    found
}

fn find_binary(kind: EngineKind, search_path: Option<&Path>) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    which::which_in(kind.binary(), search_path, cwd).ok()
}

async fn read_version(path: &Path, args: &[&str]) -> Option<String> {
    let run = Command::new(path).args(args).kill_on_drop(true).output();
    let output = tokio::time::timeout(VERSION_TIMEOUT, run)
        .await
        .ok()?
        .ok()?;
    first_meaningful_line(&String::from_utf8_lossy(&output.stdout))
}

fn first_meaningful_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_leading_blank_lines() {
        let latexmk = "\nLatexmk, John Collins, 7 Jan. 2023. Version 4.79\n";
        assert_eq!(
            first_meaningful_line(latexmk).as_deref(),
            Some("Latexmk, John Collins, 7 Jan. 2023. Version 4.79")
        );
        assert_eq!(first_meaningful_line("  \n\n"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detects_fake_tectonic_on_custom_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tectonic");
        std::fs::write(&bin, "#!/bin/sh\necho 'Tectonic 0.17.0'\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let engines = detect_engines_in(Some(dir.path())).await;

        assert_eq!(engines.len(), 1);
        assert_eq!(engines[0].kind, EngineKind::Tectonic);
        assert_eq!(engines[0].version.as_deref(), Some("Tectonic 0.17.0"));
    }

    #[tokio::test]
    async fn empty_path_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_engines_in(Some(dir.path())).await.is_empty());
    }
}
