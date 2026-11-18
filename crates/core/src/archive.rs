//! ZIP import and export of projects, and detection of the main `.tex` file.
//!
//! Import is defensive: archives come from other tools (Prism, Overleaf, arXiv) or other
//! people. Entries that would escape the project (zip-slip), symbolic links and OS junk are
//! skipped, and sizes are capped while extracting so a lying header cannot fill the disk.

use std::io::{Read, Seek, Write};
use std::path::Path;

use zip::write::SimpleFileOptions;

use crate::files::{EntryKind, list_tree};
use crate::paths::{RelPath, resolve};

#[derive(Debug, Clone, Copy)]
pub struct ImportLimits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_files: 5_000,
            max_file_bytes: 256 * 1024 * 1024,
            max_total_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("not a valid ZIP archive: {0}")]
    Invalid(String),
    #[error("the archive has more than {0} files")]
    TooManyFiles(usize),
    #[error("`{0}` is larger than the per-file limit")]
    FileTooLarge(String),
    #[error("the archive expands to more than {0} bytes")]
    TooLarge(u64),
    #[error("the archive contains no files")]
    Empty,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<zip::result::ZipError> for ArchiveError {
    fn from(err: zip::result::ZipError) -> Self {
        match err {
            zip::result::ZipError::Io(err) => Self::Io(err),
            other => Self::Invalid(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub files: usize,
    pub bytes: u64,
    /// Entries that were not extracted, with the reason.
    pub skipped: Vec<(String, &'static str)>,
    /// Best guess for the file to compile.
    pub main_file: Option<String>,
}

const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;

/// Extracts `reader` into `dest`, which must be an existing, empty directory.
pub fn import_zip<R: Read + Seek>(
    reader: R,
    dest: &Path,
    limits: ImportLimits,
) -> Result<ImportReport, ArchiveError> {
    let mut archive = zip::ZipArchive::new(reader)?;
    let mut report = ImportReport::default();

    // First pass: decide what to extract.
    let mut wanted: Vec<(usize, String)> = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        let Ok(name) = entry.name().map(|name| name.into_owned()) else {
            report
                .skipped
                .push((format!("entry #{index}"), "file name cannot be decoded"));
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & S_IFMT == S_IFLNK)
        {
            report.skipped.push((name, "symbolic link"));
            continue;
        }
        // Any `..` is suspicious even if it would stay inside the project.
        if name.split(['/', '\\']).any(|component| component == "..") {
            report.skipped.push((name, "path escapes the project"));
            continue;
        }
        let Some(safe) = entry.enclosed_name() else {
            report.skipped.push((name, "path escapes the project"));
            continue;
        };
        let Some(safe) = safe.to_str().map(|s| s.replace('\\', "/")) else {
            report.skipped.push((name, "file name is not valid UTF-8"));
            continue;
        };
        if is_junk(&safe) {
            report.skipped.push((name, "operating system metadata"));
            continue;
        }
        wanted.push((index, safe));
    }
    if wanted.is_empty() {
        return Err(ArchiveError::Empty);
    }
    if wanted.len() > limits.max_files {
        return Err(ArchiveError::TooManyFiles(limits.max_files));
    }

    // Archives made by zipping a folder put everything under one top-level directory.
    let strip = common_top_dir(wanted.iter().map(|(_, name)| name.as_str()));

    for (index, name) in wanted {
        let relative = match &strip {
            Some(prefix) => &name[prefix.len() + 1..],
            None => name.as_str(),
        };
        let Ok(rel) = RelPath::parse(relative) else {
            report.skipped.push((name, "file name is not allowed"));
            continue;
        };
        let Ok(target) = resolve(dest, &rel) else {
            report.skipped.push((name, "path escapes the project"));
            continue;
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut entry = archive.by_index(index)?;
        if entry.size() > limits.max_file_bytes {
            return Err(ArchiveError::FileTooLarge(name));
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        // Never trust the header: cap what we actually copy.
        let copied = std::io::copy(&mut (&mut entry).take(limits.max_file_bytes + 1), &mut file)?;
        if copied > limits.max_file_bytes {
            return Err(ArchiveError::FileTooLarge(name));
        }
        report.files += 1;
        report.bytes += copied;
        if report.bytes > limits.max_total_bytes {
            return Err(ArchiveError::TooLarge(limits.max_total_bytes));
        }
    }

    report.main_file = detect_main_file(dest)?;
    Ok(report)
}

fn is_junk(name: &str) -> bool {
    name.split('/').any(|component| {
        component == "__MACOSX"
            || component == ".DS_Store"
            || component == "Thumbs.db"
            || component == "desktop.ini"
            || component.starts_with("._")
    })
}

/// The single directory every name starts with, if there is one and no file sits at the top.
fn common_top_dir<'a>(names: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut common: Option<&str> = None;
    for name in names {
        let (top, _) = name.split_once('/')?;
        match common {
            None => common = Some(top),
            Some(existing) if existing == top => {}
            Some(_) => return None,
        }
    }
    common.map(str::to_owned)
}

/// Writes every file and directory of `root` into a ZIP archive.
pub fn export_zip<W: Write + Seek>(root: &Path, writer: W) -> Result<W, ArchiveError> {
    let mut zip = zip::ZipWriter::new(writer);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let entries = list_tree(root).map_err(|err| ArchiveError::Invalid(err.to_string()))?;
    for entry in entries {
        match entry.kind {
            EntryKind::Dir => zip.add_directory(format!("{}/", entry.path), options)?,
            EntryKind::File => {
                zip.start_file(entry.path.as_str(), options)?;
                let rel = RelPath::parse(&entry.path)
                    .map_err(|err| ArchiveError::Invalid(err.to_string()))?;
                let mut file = std::fs::File::open(rel.to_path(root))?;
                std::io::copy(&mut file, &mut zip)?;
            }
        }
    }
    Ok(zip.finish()?)
}

/// Finds the `.tex` file that holds `\documentclass`, preferring conventional names
/// (`main.tex`, `paper.tex`, …) and files closer to the root.
pub fn detect_main_file(root: &Path) -> Result<Option<String>, ArchiveError> {
    const PREFERRED: [&str; 8] = [
        "main",
        "paper",
        "manuscript",
        "thesis",
        "article",
        "report",
        "document",
        "ms",
    ];
    let entries = list_tree(root).map_err(|err| ArchiveError::Invalid(err.to_string()))?;
    let mut best: Option<((usize, usize, String), String)> = None;
    for entry in entries {
        if entry.kind != EntryKind::File || !entry.path.to_ascii_lowercase().ends_with(".tex") {
            continue;
        }
        let Ok(rel) = RelPath::parse(&entry.path) else {
            continue;
        };
        // \documentclass is near the top; don't read huge files whole.
        let mut head = Vec::new();
        std::fs::File::open(rel.to_path(root))?
            .take(256 * 1024)
            .read_to_end(&mut head)?;
        if !has_documentclass(&String::from_utf8_lossy(&head)) {
            continue;
        }
        let depth = entry.path.matches('/').count();
        let stem = rel.file_stem().to_ascii_lowercase();
        let preference = PREFERRED
            .iter()
            .position(|name| *name == stem)
            .unwrap_or(PREFERRED.len());
        let key = (depth, preference, entry.path.clone());
        if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
            best = Some((key, entry.path));
        }
    }
    Ok(best.map(|(_, path)| path))
}

fn has_documentclass(source: &str) -> bool {
    source.lines().any(|line| {
        let code = line.split('%').next().unwrap_or("");
        code.contains("\\documentclass")
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, options).unwrap();
            } else {
                zip.start_file(*name, options).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn imports_and_strips_a_common_top_dir() {
        let data = build_zip(&[
            ("paper/", b""),
            ("paper/main.tex", b"\\documentclass{article}"),
            ("paper/sections/intro.tex", b"Intro"),
            ("__MACOSX/paper/._main.tex", b"junk"),
            ("paper/.DS_Store", b"junk"),
        ]);
        let dir = tempfile::tempdir().unwrap();

        let report = import_zip(Cursor::new(data), dir.path(), ImportLimits::default()).unwrap();

        assert_eq!(report.files, 2);
        assert_eq!(report.main_file.as_deref(), Some("main.tex"));
        assert_eq!(report.skipped.len(), 2);
        assert!(dir.path().join("sections/intro.tex").is_file());
    }

    #[test]
    fn skips_zip_slip_entries() {
        let data = build_zip(&[
            ("ok.tex", b"\\documentclass{article}"),
            ("../evil.tex", b"x"),
            ("/abs/evil.tex", b"x"),
        ]);
        let outer = tempfile::tempdir().unwrap();
        let dest = outer.path().join("project");
        std::fs::create_dir(&dest).unwrap();

        let report = import_zip(Cursor::new(data), &dest, ImportLimits::default()).unwrap();

        // `../evil.tex` is dropped; the absolute name is re-rooted inside the project.
        assert!(report.skipped.iter().any(|(name, _)| name == "../evil.tex"));
        assert!(!outer.path().join("evil.tex").exists());
        assert!(!Path::new("/abs/evil.tex").exists());
        assert!(dest.join("ok.tex").is_file());
        for file in walk_files(outer.path()) {
            assert!(file.starts_with(&dest), "{} escaped", file.display());
        }
    }

    fn walk_files(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(walk_files(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinks() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.add_symlink("passwd", "/etc/passwd", SimpleFileOptions::default())
            .unwrap();
        zip.start_file("main.tex", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"\\documentclass{article}").unwrap();
        let data = zip.finish().unwrap().into_inner();
        let dir = tempfile::tempdir().unwrap();

        let report = import_zip(Cursor::new(data), dir.path(), ImportLimits::default()).unwrap();

        assert_eq!(report.files, 1);
        assert!(!dir.path().join("passwd").exists());
    }

    #[test]
    fn enforces_limits() {
        let data = build_zip(&[("a.tex", &[b'x'; 2048]), ("b.tex", b"y")]);
        let dir = tempfile::tempdir().unwrap();
        let limits = ImportLimits {
            max_files: 10,
            max_file_bytes: 1024,
            max_total_bytes: 10_000,
        };
        assert!(matches!(
            import_zip(Cursor::new(data.clone()), dir.path(), limits),
            Err(ArchiveError::FileTooLarge(_))
        ));
        let dir = tempfile::tempdir().unwrap();
        let limits = ImportLimits {
            max_files: 1,
            ..ImportLimits::default()
        };
        assert!(matches!(
            import_zip(Cursor::new(data), dir.path(), limits),
            Err(ArchiveError::TooManyFiles(1))
        ));
        assert!(matches!(
            import_zip(Cursor::new(b"not a zip".to_vec()), dir.path(), limits),
            Err(ArchiveError::Invalid(_))
        ));
    }

    #[test]
    fn export_round_trips() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("figs/empty")).unwrap();
        std::fs::write(src.path().join("main.tex"), "\\documentclass{book}").unwrap();
        std::fs::write(src.path().join("figs/a.txt"), "a").unwrap();

        let data = export_zip(src.path(), Cursor::new(Vec::new()))
            .unwrap()
            .into_inner();
        let dest = tempfile::tempdir().unwrap();
        let report = import_zip(Cursor::new(data), dest.path(), ImportLimits::default()).unwrap();

        assert_eq!(report.files, 2);
        assert_eq!(
            std::fs::read_to_string(dest.path().join("figs/a.txt")).unwrap(),
            "a"
        );
    }

    #[test]
    fn main_file_detection_prefers_conventional_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("old")).unwrap();
        std::fs::write(root.join("appendix.tex"), "% \\documentclass{x}\nnot main").unwrap();
        std::fs::write(root.join("zzz.tex"), "\\documentclass{article}").unwrap();
        std::fs::write(root.join("paper.tex"), "\\documentclass{article}").unwrap();
        std::fs::write(root.join("old/main.tex"), "\\documentclass{article}").unwrap();

        assert_eq!(
            detect_main_file(root).unwrap().as_deref(),
            Some("paper.tex")
        );
        std::fs::remove_file(root.join("paper.tex")).unwrap();
        std::fs::remove_file(root.join("zzz.tex")).unwrap();
        assert_eq!(
            detect_main_file(root).unwrap().as_deref(),
            Some("old/main.tex")
        );
    }
}
