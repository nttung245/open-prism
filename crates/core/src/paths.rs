//! Paths inside a project.
//!
//! Every path that comes from a client is parsed into a [`RelPath`] first: relative,
//! `/`-separated, without `.`/`..`/empty components. [`resolve`] then joins it to the project
//! root and refuses to go through any symbolic link, so a request can never leave the project.

use std::fmt;
use std::path::{Path, PathBuf};

/// Longest accepted relative path, in bytes.
const MAX_PATH_LEN: usize = 1024;
/// Longest accepted path component, in bytes (the usual file-system limit).
const MAX_COMPONENT_LEN: usize = 255;

/// Prefix of temporary files written by [`crate::files::write`]; hidden from listings.
pub const TEMP_PREFIX: &str = ".~openprism-";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is empty")]
    Empty,
    #[error("path must be relative to the project")]
    Absolute,
    #[error("path is too long")]
    TooLong,
    #[error("path component `{0}` is not allowed")]
    BadComponent(String),
    #[error("path goes through a symbolic link")]
    Symlink,
}

/// A validated, normalised path relative to a project root, e.g. `chapters/intro.tex`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        if raw.is_empty() {
            return Err(PathError::Empty);
        }
        if raw.starts_with('/') || raw.starts_with('\\') || has_drive_prefix(raw) {
            return Err(PathError::Absolute);
        }
        if raw.len() > MAX_PATH_LEN {
            return Err(PathError::TooLong);
        }
        // A single trailing slash is harmless ("figures/"); anything else must be clean.
        let raw = raw.strip_suffix('/').unwrap_or(raw);
        for component in raw.split('/') {
            let bad = component.is_empty()
                || component == "."
                || component == ".."
                || component.len() > MAX_COMPONENT_LEN
                || component.starts_with(TEMP_PREFIX)
                || component
                    .chars()
                    .any(|c| c == '\\' || c == '\0' || c.is_control());
            if bad {
                return Err(PathError::BadComponent(component.to_owned()));
            }
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Last component, e.g. `intro.tex`.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// File name without its last extension, e.g. `main` for `src/main.tex`.
    pub fn file_stem(&self) -> &str {
        let name = self.file_name();
        match name.rfind('.') {
            Some(0) | None => name,
            Some(dot) => &name[..dot],
        }
    }

    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name();
        name.rfind('.')
            .filter(|&dot| dot > 0)
            .map(|dot| &name[dot + 1..])
    }

    pub fn parent(&self) -> Option<RelPath> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| Self(parent.to_owned()))
    }

    /// `self/child`, validating `child`.
    pub fn join(&self, child: &str) -> Result<RelPath, PathError> {
        Self::parse(&format!("{}/{child}", self.0))
    }

    /// Whether `self` is `other` or lies inside it.
    pub fn starts_with(&self, other: &RelPath) -> bool {
        self.0 == other.0
            || (self.0.starts_with(&other.0) && self.0.as_bytes().get(other.0.len()) == Some(&b'/'))
    }

    /// Joins to `root` without any file-system check. Prefer [`resolve`].
    pub fn to_path(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        path.extend(self.0.split('/'));
        path
    }

    /// Converts a path below `root` (as found on disk) back into a `RelPath`.
    pub fn from_path(root: &Path, path: &Path) -> Option<RelPath> {
        let relative = path.strip_prefix(root).ok()?;
        let joined = relative
            .components()
            .map(|c| c.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()?
            .join("/");
        Self::parse(&joined).ok()
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn has_drive_prefix(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Joins `rel` to `root`, failing if an existing component on the way is a symbolic link.
/// Components that do not exist yet are fine (the caller may be creating them).
pub fn resolve(root: &Path, rel: &RelPath) -> Result<PathBuf, PathError> {
    let mut path = root.to_path_buf();
    for component in rel.as_str().split('/') {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(PathError::Symlink),
            Ok(_) => {}
            // Nothing exists below a missing component, so no symlink can follow.
            Err(_) => break,
        }
    }
    Ok(rel.to_path(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        for ok in [
            "main.tex",
            "chapters/intro.tex",
            "figures/",
            "a b/ç.tex",
            ".latexmkrc",
        ] {
            assert!(RelPath::parse(ok).is_ok(), "{ok}");
        }
        assert_eq!(RelPath::parse("figures/").unwrap().as_str(), "figures");
    }

    #[test]
    fn rejects_escapes_and_oddities() {
        for bad in [
            "",
            "/etc/passwd",
            "\\server\\share",
            "C:/Windows",
            "../secret",
            "a/../../b",
            "a/./b",
            "a//b",
            "a\\..\\b",
            "nul\0byte",
            "line\nbreak",
            ".~openprism-123",
        ] {
            assert!(RelPath::parse(bad).is_err(), "{bad:?}");
        }
        assert!(RelPath::parse(&"a/".repeat(600)).is_err());
    }

    #[test]
    fn name_helpers() {
        let path = RelPath::parse("src/main.tex").unwrap();
        assert_eq!(path.file_name(), "main.tex");
        assert_eq!(path.file_stem(), "main");
        assert_eq!(path.extension(), Some("tex"));
        assert_eq!(path.parent().unwrap().as_str(), "src");
        assert_eq!(RelPath::parse(".latexmkrc").unwrap().extension(), None);
        assert!(path.starts_with(&RelPath::parse("src").unwrap()));
        assert!(!path.starts_with(&RelPath::parse("sr").unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink("/etc", root.join("link")).unwrap();

        let ok = RelPath::parse("real/new.tex").unwrap();
        assert_eq!(resolve(&root, &ok).unwrap(), root.join("real/new.tex"));
        for bad in ["link", "link/passwd"] {
            let rel = RelPath::parse(bad).unwrap();
            assert_eq!(resolve(&root, &rel), Err(PathError::Symlink));
        }
    }

    #[test]
    fn round_trips_disk_paths() {
        let root = Path::new("/data/projects/x");
        let rel = RelPath::from_path(root, &root.join("a/b.tex")).unwrap();
        assert_eq!(rel.as_str(), "a/b.tex");
        assert!(RelPath::from_path(root, Path::new("/elsewhere/b.tex")).is_none());
    }
}
