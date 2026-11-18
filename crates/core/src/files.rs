//! File operations inside a project directory.
//!
//! All functions are synchronous (`std::fs`); async callers run them on a blocking thread and
//! serialise writes per project so that the version check and the write happen atomically.
//!
//! A file's *version* is a hash of its content. It needs no bookkeeping, survives restarts and
//! stays correct when the file is changed by another program.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::paths::{PathError, RelPath, TEMP_PREFIX, resolve};

/// Most entries returned by [`list_tree`]; bigger projects are almost certainly a mistake
/// (e.g. a `node_modules` folder) and would make the UI unusable anyway.
pub const MAX_TREE_ENTRIES: usize = 20_000;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("`{0}` does not exist")]
    NotFound(String),
    #[error("`{0}` already exists")]
    AlreadyExists(String),
    #[error("`{0}` is a directory")]
    IsDirectory(String),
    #[error("`{0}` is not a directory")]
    NotDirectory(String),
    #[error("the file changed since it was loaded")]
    VersionConflict { current: Option<String> },
    #[error("the project has more than {MAX_TREE_ENTRIES} files")]
    TooManyEntries,
    #[error("cannot move `{0}` into itself")]
    IntoItself(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum EntryKind {
    File,
    Dir,
}

/// One file or directory of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FileEntry {
    /// Relative to the project root, `/`-separated.
    pub path: String,
    pub kind: EntryKind,
    /// Size in bytes (0 for directories).
    pub size: u64,
}

/// Condition checked before a write (from `If-Match` / `If-None-Match`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// Write unconditionally.
    Any,
    /// The file must currently have this version.
    Version(String),
    /// The file must not exist yet.
    Absent,
}

/// Content hash used as the file version: the first 128 bits of SHA-256, in hex.
pub fn version_of(bytes: &[u8]) -> String {
    hex::encode(&Sha256::digest(bytes)[..16])
}

/// Every file and directory below `root`, sorted by path. Symbolic links and temporary files
/// are skipped.
pub fn list_tree(root: &Path) -> Result<Vec<FileEntry>, FileError> {
    let mut entries = Vec::new();
    walk(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<FileEntry>) -> Result<(), FileError> {
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        let file_type = item.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = item.path();
        let Some(rel) = RelPath::from_path(root, &path) else {
            // Names we cannot represent (not UTF-8, temp files): not part of the project.
            continue;
        };
        if out.len() >= MAX_TREE_ENTRIES {
            return Err(FileError::TooManyEntries);
        }
        if file_type.is_dir() {
            out.push(FileEntry {
                path: rel.as_str().to_owned(),
                kind: EntryKind::Dir,
                size: 0,
            });
            walk(root, &path, out)?;
        } else if file_type.is_file() {
            out.push(FileEntry {
                path: rel.as_str().to_owned(),
                kind: EntryKind::File,
                size: item.metadata()?.len(),
            });
        }
    }
    Ok(())
}

/// Reads a file and returns its content and version.
pub fn read(root: &Path, rel: &RelPath) -> Result<(Vec<u8>, String), FileError> {
    let path = resolve(root, rel)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let version = version_of(&bytes);
            Ok((bytes, version))
        }
        Err(err) => Err(map_missing(err, rel, &path)),
    }
}

/// Current version of a file, or `None` if it does not exist.
pub fn current_version(root: &Path, rel: &RelPath) -> Result<Option<String>, FileError> {
    match read(root, rel) {
        Ok((_, version)) => Ok(Some(version)),
        Err(FileError::NotFound(_)) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Writes a file atomically (temporary file + rename) after checking `precondition`, creating
/// parent directories as needed. Returns the new version.
pub fn write(
    root: &Path,
    rel: &RelPath,
    bytes: &[u8],
    precondition: &Precondition,
) -> Result<String, FileError> {
    let path = resolve(root, rel)?;
    if path.is_dir() {
        return Err(FileError::IsDirectory(rel.to_string()));
    }
    match precondition {
        Precondition::Any => {}
        Precondition::Absent => {
            if path.exists() {
                return Err(FileError::AlreadyExists(rel.to_string()));
            }
        }
        Precondition::Version(expected) => {
            let current = current_version(root, rel)?;
            if current.as_deref() != Some(expected.as_str()) {
                return Err(FileError::VersionConflict { current });
            }
        }
    }

    let parent = match rel.parent() {
        Some(parent) => {
            let dir = resolve(root, &parent)?;
            std::fs::create_dir_all(&dir)?;
            dir
        }
        None => root.to_path_buf(),
    };
    let mut temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(&parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(&path).map_err(|err| err.error)?;
    Ok(version_of(bytes))
}

/// Creates a directory (and its parents). Fails if something already exists at `rel`.
pub fn create_dir(root: &Path, rel: &RelPath) -> Result<(), FileError> {
    let path = resolve(root, rel)?;
    if path.exists() {
        return Err(FileError::AlreadyExists(rel.to_string()));
    }
    std::fs::create_dir_all(&path)?;
    Ok(())
}

/// Moves a file or directory. Fails if `to` already exists.
pub fn rename(root: &Path, from: &RelPath, to: &RelPath) -> Result<(), FileError> {
    if to.starts_with(from) {
        return Err(FileError::IntoItself(from.to_string()));
    }
    let source = resolve(root, from)?;
    let target = resolve(root, to)?;
    if !source.exists() {
        return Err(FileError::NotFound(from.to_string()));
    }
    if target.exists() {
        return Err(FileError::AlreadyExists(to.to_string()));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(resolve(root, &parent)?)?;
    }
    std::fs::rename(&source, &target)?;
    Ok(())
}

/// Deletes a file, or a directory with everything in it.
pub fn delete(root: &Path, rel: &RelPath) -> Result<(), FileError> {
    let path = resolve(root, rel)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|err| map_missing(err, rel, &path))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(&path)?;
    } else {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

fn map_missing(err: std::io::Error, rel: &RelPath, path: &Path) -> FileError {
    match err.kind() {
        std::io::ErrorKind::NotFound => FileError::NotFound(rel.to_string()),
        std::io::ErrorKind::IsADirectory => FileError::IsDirectory(rel.to_string()),
        std::io::ErrorKind::NotADirectory => FileError::NotDirectory(rel.to_string()),
        _ if path.is_dir() => FileError::IsDirectory(rel.to_string()),
        _ => FileError::Io(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::parse(path).unwrap()
    }

    #[test]
    fn write_read_and_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let path = rel("chapters/intro.tex");

        let v1 = write(root, &path, b"one", &Precondition::Absent).unwrap();
        assert_eq!(read(root, &path).unwrap(), (b"one".to_vec(), v1.clone()));
        assert!(matches!(
            write(root, &path, b"again", &Precondition::Absent),
            Err(FileError::AlreadyExists(_))
        ));

        let v2 = write(root, &path, b"two", &Precondition::Version(v1.clone())).unwrap();
        assert_ne!(v1, v2);
        match write(root, &path, b"stale", &Precondition::Version(v1)) {
            Err(FileError::VersionConflict { current }) => assert_eq!(current, Some(v2)),
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(read(root, &path).unwrap().0, b"two");
    }

    #[test]
    fn tree_lists_files_and_dirs_without_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, &rel("main.tex"), b"x", &Precondition::Any).unwrap();
        write(root, &rel("figs/a.png"), b"png", &Precondition::Any).unwrap();
        std::fs::write(root.join(format!("{TEMP_PREFIX}abc")), b"tmp").unwrap();

        let tree = list_tree(root).unwrap();
        let paths: Vec<_> = tree.iter().map(|e| (e.path.as_str(), e.kind)).collect();
        assert_eq!(
            paths,
            [
                ("figs", EntryKind::Dir),
                ("figs/a.png", EntryKind::File),
                ("main.tex", EntryKind::File)
            ]
        );
        assert_eq!(tree[1].size, 3);
    }

    #[test]
    fn rename_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, &rel("a/b.tex"), b"x", &Precondition::Any).unwrap();

        rename(root, &rel("a"), &rel("c/d")).unwrap();
        assert!(root.join("c/d/b.tex").is_file());
        assert!(matches!(
            rename(root, &rel("c"), &rel("c/d/e")),
            Err(FileError::IntoItself(_))
        ));
        assert!(matches!(
            rename(root, &rel("missing"), &rel("x")),
            Err(FileError::NotFound(_))
        ));

        delete(root, &rel("c")).unwrap();
        assert!(!root.join("c").exists());
        assert!(matches!(
            delete(root, &rel("c")),
            Err(FileError::NotFound(_))
        ));
    }

    #[test]
    fn reading_a_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        create_dir(dir.path(), &rel("figs")).unwrap();
        assert!(matches!(
            read(dir.path(), &rel("figs")),
            Err(FileError::IsDirectory(_))
        ));
        assert!(matches!(
            create_dir(dir.path(), &rel("figs")),
            Err(FileError::AlreadyExists(_))
        ));
    }
}
