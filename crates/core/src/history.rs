//! Version history of a project: snapshots kept in a bare git repository outside the project
//! directory (`<data dir>/history/<project id>.git`).
//!
//! Snapshots are built by reading the project directory directly, without a git index or work
//! tree, using the same rules as the editor: symbolic links and Open Prism's temporary files are
//! skipped. So is anything named `.git`, at any depth: it belongs to the user (their own
//! repository, a submodule) and git cannot store that name anyway. Files bigger than a limit are
//! not recorded either. Restoring writes files with [`files::write`], so writes
//! are atomic and never follow symbolic links.
//!
//! History is linear (`refs/heads/main`) and only ever grows: restoring creates new snapshots.
//! Every commit message ends with an `Open-Prism-Kind:` trailer so the repository can be
//! understood without the database.
//!
//! All functions are synchronous; async callers run them on a blocking thread.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use git2::{
    DiffOptions, ErrorCode, ObjectType, Oid, Patch, Repository, RepositoryInitOptions, Signature,
    TreeWalkMode, TreeWalkResult,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::files::{self, FileError, MAX_TREE_ENTRIES, Precondition};
use crate::paths::RelPath;

/// Files bigger than this are not recorded.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Text files bigger than this are reported as changed, without a line diff.
pub const MAX_DIFF_BYTES: u64 = 1024 * 1024;

const BRANCH: &str = "main";
const KIND_TRAILER: &str = "Open-Prism-Kind";
const AUTHOR_NAME: &str = "Open Prism";
const AUTHOR_EMAIL: &str = "open-prism@localhost";
const CONTEXT_LINES: u32 = 3;
/// Bytes inspected to tell binary from text: a NUL byte among them means binary, like git.
const BINARY_PROBE: usize = 8000;
const FILE_MODE: i32 = 0o100_644;
const DIR_MODE: i32 = 0o040_000;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Git(#[from] git2::Error),
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("unknown revision `{0}`")]
    UnknownRevision(String),
    #[error("`{0}` is in neither version")]
    NoSuchFile(String),
    #[error("the project has more than {MAX_TREE_ENTRIES} files")]
    TooManyFiles,
}

impl From<crate::paths::PathError> for HistoryError {
    fn from(err: crate::paths::PathError) -> Self {
        Self::File(FileError::Path(err))
    }
}

/// Why a snapshot was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SnapshotKind {
    /// The project was created or imported.
    Created,
    /// The assistant proposed edits; this is the state before they were applied.
    BeforeAiEdit,
    /// Edits of the assistant were accepted.
    AiEdit,
    /// A compilation succeeded.
    Compiled,
    /// Saved by the user, usually with a label.
    Manual,
    /// The state just before a restore.
    BeforeRestore,
    /// The state just after a restore.
    Restored,
}

impl SnapshotKind {
    pub const ALL: [Self; 7] = [
        Self::Created,
        Self::BeforeAiEdit,
        Self::AiEdit,
        Self::Compiled,
        Self::Manual,
        Self::BeforeRestore,
        Self::Restored,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::BeforeAiEdit => "beforeAiEdit",
            Self::AiEdit => "aiEdit",
            Self::Compiled => "compiled",
            Self::Manual => "manual",
            Self::BeforeRestore => "beforeRestore",
            Self::Restored => "restored",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// Number of files added, modified and deleted between two versions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangeCounts {
    pub added: u32,
    pub modified: u32,
    pub deleted: u32,
}

/// What [`History::snapshot`] records.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotRequest<'a> {
    pub kind: SnapshotKind,
    /// One line describing the snapshot.
    pub summary: &'a str,
    /// Record a snapshot even if nothing changed since the last one.
    pub force: bool,
    /// Files bigger than this are skipped ([`MAX_FILE_BYTES`] outside tests).
    pub max_file_bytes: u64,
    /// The git author: the display name of whoever led to the snapshot (server mode). The
    /// committer is always Open Prism; `None` makes Open Prism the author too.
    pub author: Option<&'a str>,
}

/// A commit made by [`History::snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCommit {
    pub rev: String,
    /// Changes compared with the previous snapshot.
    pub counts: ChangeCounts,
}

/// A snapshot as read back from the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub rev: String,
    /// `None` for commits not made by Open Prism.
    pub kind: Option<SnapshotKind>,
    pub summary: String,
    /// Commit time, in milliseconds since the Unix epoch (second precision).
    pub time_ms: i64,
    pub counts: ChangeCounts,
}

/// The version a snapshot is compared with. Changes always go from this version to the
/// snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Against<'a> {
    /// The previous snapshot (an empty project for the first one).
    Parent,
    /// The files currently in the project directory.
    Current,
    /// Another snapshot.
    Rev(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
}

/// A file that differs between two versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangedFile {
    pub path: String,
    pub status: ChangeStatus,
    /// Lines added; `None` for binary files and files too large to compare.
    pub additions: Option<u32>,
    /// Lines removed; `None` for binary files and files too large to compare.
    pub deletions: Option<u32>,
    pub binary: bool,
    /// Size in the older version, `None` if the file is not in it.
    pub old_size: Option<u64>,
    /// Size in the newer version, `None` if the file is not in it.
    pub new_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line without its line break.
    pub text: String,
}

/// A run of changed lines with a few lines of context around them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

/// Line diff of one file between two versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FileDiff {
    pub path: String,
    pub binary: bool,
    /// One of the versions is bigger than [`MAX_DIFF_BYTES`]: no line diff.
    pub too_large: bool,
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
    /// Empty for binary files, files too large to compare and identical files.
    pub hunks: Vec<DiffHunk>,
}

/// The history repository of one project.
pub struct History {
    repo: Repository,
}

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History")
            .field("path", &self.repo.path())
            .finish()
    }
}

impl History {
    /// Opens the repository at `git_dir`, creating an empty one if there is none yet.
    pub fn open_or_init(git_dir: &Path) -> Result<Self, HistoryError> {
        if let Some(history) = Self::open(git_dir)? {
            return Ok(history);
        }
        let mut options = RepositoryInitOptions::new();
        options.bare(true).initial_head(BRANCH).mkpath(true);
        Ok(Self {
            repo: Repository::init_opts(git_dir, &options)?,
        })
    }

    /// Opens an existing repository; `None` if the project has no history yet.
    pub fn open(git_dir: &Path) -> Result<Option<Self>, HistoryError> {
        if !git_dir.exists() {
            return Ok(None);
        }
        Ok(Some(Self {
            repo: Repository::open_bare(git_dir)?,
        }))
    }

    /// The latest snapshot, if any.
    pub fn head(&self) -> Result<Option<String>, HistoryError> {
        Ok(self.head_commit()?.map(|commit| commit.id().to_string()))
    }

    /// Records the files of `project_root`. Returns `None` when nothing changed since the
    /// latest snapshot and `request.force` is not set.
    pub fn snapshot(
        &self,
        project_root: &Path,
        request: SnapshotRequest<'_>,
    ) -> Result<Option<SnapshotCommit>, HistoryError> {
        let files = scan(project_root, request.max_file_bytes)?;
        let mut entries = BTreeMap::new();
        for (path, absolute) in &files.tracked {
            entries.insert(path.clone(), self.repo.blob_path(absolute)?);
        }
        let tree_id = write_tree(&self.repo, &entries)?;
        let parent = self.head_commit()?;
        if !request.force
            && parent
                .as_ref()
                .is_some_and(|commit| commit.tree_id() == tree_id)
        {
            return Ok(None);
        }
        let before = match &parent {
            Some(commit) => tree_entries(&commit.tree()?)?,
            None => BTreeMap::new(),
        };
        let counts = count_changes(&before, &entries);
        let tree = self.repo.find_tree(tree_id)?;
        let committer = Signature::now(AUTHOR_NAME, AUTHOR_EMAIL)?;
        let author = Signature::now(request.author.unwrap_or(AUTHOR_NAME), AUTHOR_EMAIL)?;
        let message = commit_message(request.kind, request.summary);
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        let id = self
            .repo
            .commit(Some("HEAD"), &author, &committer, &message, &tree, &parents)?;
        Ok(Some(SnapshotCommit {
            rev: id.to_string(),
            counts,
        }))
    }

    /// Snapshots from the latest one backwards, stopping before the first one `is_known`
    /// accepts. Newest first.
    pub fn unrecorded(
        &self,
        is_known: impl Fn(&str) -> bool,
    ) -> Result<Vec<CommitInfo>, HistoryError> {
        let mut found = Vec::new();
        let mut next = self.head_commit()?;
        while let Some(commit) = next {
            if is_known(&commit.id().to_string()) {
                break;
            }
            found.push(self.info(&commit)?);
            next = commit.parents().next();
        }
        Ok(found)
    }

    /// Files that differ between `against` and the snapshot `rev`, sorted by path.
    pub fn changes(
        &self,
        rev: &str,
        against: Against<'_>,
        project_root: &Path,
        max_file_bytes: u64,
    ) -> Result<Vec<ChangedFile>, HistoryError> {
        let (old, new) = self.sides(rev, against, project_root, max_file_bytes)?;
        let paths: BTreeSet<&String> = old.entries.keys().chain(new.entries.keys()).collect();
        let mut changes = Vec::new();
        for path in paths {
            let status = match (old.entries.get(path), new.entries.get(path)) {
                (Some(a), Some(b)) if a == b => continue,
                (Some(_), Some(_)) => ChangeStatus::Modified,
                (None, Some(_)) => ChangeStatus::Added,
                (Some(_), None) => ChangeStatus::Deleted,
                (None, None) => continue,
            };
            let old_bytes = old.read(path)?;
            let new_bytes = new.read(path)?;
            let comparison = compare(old_bytes.as_deref(), new_bytes.as_deref(), false)?;
            changes.push(ChangedFile {
                path: path.clone(),
                status,
                additions: comparison.stats.map(|(added, _)| added),
                deletions: comparison.stats.map(|(_, removed)| removed),
                binary: comparison.binary,
                old_size: old_bytes.as_ref().map(|bytes| bytes.len() as u64),
                new_size: new_bytes.as_ref().map(|bytes| bytes.len() as u64),
            });
        }
        Ok(changes)
    }

    /// Line diff of `path` from `against` to the snapshot `rev`.
    pub fn file_diff(
        &self,
        rev: &str,
        against: Against<'_>,
        path: &RelPath,
        project_root: &Path,
        max_file_bytes: u64,
    ) -> Result<FileDiff, HistoryError> {
        let (old, new) = self.sides(rev, against, project_root, max_file_bytes)?;
        let old_bytes = old.read(path.as_str())?;
        let new_bytes = new.read(path.as_str())?;
        if old_bytes.is_none() && new_bytes.is_none() {
            return Err(HistoryError::NoSuchFile(path.to_string()));
        }
        let comparison = compare(old_bytes.as_deref(), new_bytes.as_deref(), true)?;
        Ok(FileDiff {
            path: path.to_string(),
            binary: comparison.binary,
            too_large: comparison.too_large,
            old_size: old_bytes.as_ref().map(|bytes| bytes.len() as u64),
            new_size: new_bytes.as_ref().map(|bytes| bytes.len() as u64),
            hunks: comparison.hunks,
        })
    }

    /// Makes the project match the snapshot `rev`: every file of `paths` (all recorded files
    /// when `None`) gets its content from the snapshot, and files absent from it are deleted.
    /// Files that are not recorded (`.git`, symbolic links, files over the size limit) are
    /// never touched. Returns the paths that changed, sorted.
    pub fn restore(
        &self,
        rev: &str,
        project_root: &Path,
        paths: Option<&[RelPath]>,
        max_file_bytes: u64,
    ) -> Result<Vec<String>, HistoryError> {
        let target = tree_entries(&self.commit(rev)?.tree()?)?;
        let files = scan(project_root, max_file_bytes)?;
        let mut current = BTreeMap::new();
        for (path, absolute) in &files.tracked {
            current.insert(path.clone(), Oid::hash_file(ObjectType::Blob, absolute)?);
        }
        let candidates: BTreeSet<String> = match paths {
            Some(paths) => paths.iter().map(|path| path.as_str().to_owned()).collect(),
            None => target.keys().chain(current.keys()).cloned().collect(),
        };
        let mut changed = Vec::new();
        for path in candidates {
            if files.oversized.contains(&path) {
                continue;
            }
            let rel = RelPath::parse(&path)?;
            match (target.get(&path), current.get(&path)) {
                (Some(wanted), Some(present)) if wanted == present => {}
                (Some(wanted), _) => {
                    let blob = self.repo.find_blob(*wanted)?;
                    files::write(project_root, &rel, blob.content(), &Precondition::Any)?;
                    changed.push(path);
                }
                (None, Some(_)) => {
                    files::delete(project_root, &rel)?;
                    changed.push(path);
                }
                (None, None) => {}
            }
        }
        Ok(changed)
    }

    fn head_commit(&self) -> Result<Option<git2::Commit<'_>>, HistoryError> {
        match self.repo.head() {
            Ok(reference) => Ok(Some(reference.peel_to_commit()?)),
            Err(err) if matches!(err.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => {
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn commit(&self, rev: &str) -> Result<git2::Commit<'_>, HistoryError> {
        let unknown = || HistoryError::UnknownRevision(rev.to_owned());
        if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(unknown());
        }
        let id = Oid::from_str(rev).map_err(|_| unknown())?;
        self.repo.find_commit(id).map_err(|err| match err.code() {
            ErrorCode::NotFound => unknown(),
            _ => err.into(),
        })
    }

    fn info(&self, commit: &git2::Commit<'_>) -> Result<CommitInfo, HistoryError> {
        let (summary, kind) = parse_message(commit.message().unwrap_or_default());
        let before = match commit.parents().next() {
            Some(parent) => tree_entries(&parent.tree()?)?,
            None => BTreeMap::new(),
        };
        let after = tree_entries(&commit.tree()?)?;
        Ok(CommitInfo {
            rev: commit.id().to_string(),
            kind,
            summary,
            time_ms: commit.time().seconds() * 1000,
            counts: count_changes(&before, &after),
        })
    }

    /// The older and the newer version of a comparison.
    fn sides(
        &self,
        rev: &str,
        against: Against<'_>,
        project_root: &Path,
        max_file_bytes: u64,
    ) -> Result<(Side<'_>, Side<'_>), HistoryError> {
        let commit = self.commit(rev)?;
        let new = self.tree_side(Some(&commit))?;
        let old = match against {
            Against::Parent => self.tree_side(commit.parents().next().as_ref())?,
            Against::Rev(other) => self.tree_side(Some(&self.commit(other)?))?,
            Against::Current => {
                let files = scan(project_root, max_file_bytes)?;
                let mut entries = BTreeMap::new();
                for (path, absolute) in &files.tracked {
                    entries.insert(path.clone(), Oid::hash_file(ObjectType::Blob, absolute)?);
                }
                Side {
                    entries,
                    source: Source::Disk(files.tracked),
                }
            }
        };
        Ok((old, new))
    }

    fn tree_side(&self, commit: Option<&git2::Commit<'_>>) -> Result<Side<'_>, HistoryError> {
        let entries = match commit {
            Some(commit) => tree_entries(&commit.tree()?)?,
            None => BTreeMap::new(),
        };
        Ok(Side {
            entries,
            source: Source::Repo(&self.repo),
        })
    }

    #[cfg(test)]
    fn object_count(&self) -> usize {
        let mut count = 0;
        if let Ok(odb) = self.repo.odb() {
            let _ = odb.foreach(|_| {
                count += 1;
                true
            });
        }
        count
    }
}

/// One version in a comparison: path → blob id, and where the content can be read.
struct Side<'r> {
    entries: BTreeMap<String, Oid>,
    source: Source<'r>,
}

enum Source<'r> {
    Repo(&'r Repository),
    /// Path → absolute path of the files on disk.
    Disk(BTreeMap<String, PathBuf>),
}

impl Side<'_> {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, HistoryError> {
        let Some(id) = self.entries.get(path) else {
            return Ok(None);
        };
        match &self.source {
            Source::Repo(repo) => Ok(Some(repo.find_blob(*id)?.content().to_vec())),
            Source::Disk(files) => match files.get(path) {
                Some(absolute) => Ok(Some(std::fs::read(absolute)?)),
                None => Ok(None),
            },
        }
    }
}

/// Files of a project directory, by the rules of the module documentation.
struct Scan {
    /// Recorded files: path → absolute path.
    tracked: BTreeMap<String, PathBuf>,
    /// Regular files skipped because they are too big.
    oversized: BTreeSet<String>,
}

fn scan(root: &Path, max_file_bytes: u64) -> Result<Scan, HistoryError> {
    let mut scan = Scan {
        tracked: BTreeMap::new(),
        oversized: BTreeSet::new(),
    };
    let mut seen = 0;
    walk(root, root, max_file_bytes, &mut scan, &mut seen)?;
    Ok(scan)
}

fn walk(
    root: &Path,
    dir: &Path,
    max_file_bytes: u64,
    scan: &mut Scan,
    seen: &mut usize,
) -> Result<(), HistoryError> {
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        let file_type = item.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = item.path();
        // Names we cannot represent (not UTF-8, temporary files) are not part of the project.
        let Some(rel) = RelPath::from_path(root, &path) else {
            continue;
        };
        if is_git_name(rel.file_name()) {
            continue;
        }
        *seen += 1;
        if *seen > MAX_TREE_ENTRIES {
            return Err(HistoryError::TooManyFiles);
        }
        if file_type.is_dir() {
            walk(root, &path, max_file_bytes, scan, seen)?;
        } else if file_type.is_file() {
            if item.metadata()?.len() > max_file_bytes {
                scan.oversized.insert(rel.as_str().to_owned());
            } else {
                scan.tracked.insert(rel.as_str().to_owned(), path);
            }
        }
    }
    Ok(())
}

/// Names git refuses to store in a tree: `.git` in any letter case, and the forms Windows
/// and macOS file systems treat as the same name (`git~1`, `.git.`, `.git `).
fn is_git_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "git~1"
        || lower
            .strip_prefix(".git")
            .is_some_and(|rest| rest.chars().all(|c| c == '.' || c == ' '))
}

/// A directory being turned into a git tree.
#[derive(Default)]
struct DirNode {
    files: BTreeMap<String, Oid>,
    dirs: BTreeMap<String, DirNode>,
}

impl DirNode {
    fn insert(&mut self, path: &str, id: Oid) {
        match path.split_once('/') {
            Some((head, rest)) => self
                .dirs
                .entry(head.to_owned())
                .or_default()
                .insert(rest, id),
            None => {
                self.files.insert(path.to_owned(), id);
            }
        }
    }

    fn write(&self, repo: &Repository) -> Result<Oid, HistoryError> {
        let mut builder = repo.treebuilder(None)?;
        for (name, dir) in &self.dirs {
            builder.insert(name, dir.write(repo)?, DIR_MODE)?;
        }
        for (name, id) in &self.files {
            builder.insert(name, *id, FILE_MODE)?;
        }
        Ok(builder.write()?)
    }
}

fn write_tree(repo: &Repository, entries: &BTreeMap<String, Oid>) -> Result<Oid, HistoryError> {
    let mut root = DirNode::default();
    for (path, id) in entries {
        root.insert(path, *id);
    }
    root.write(repo)
}

/// Every file of a tree: path → blob id.
fn tree_entries(tree: &git2::Tree<'_>) -> Result<BTreeMap<String, Oid>, HistoryError> {
    let mut entries = BTreeMap::new();
    tree.walk(TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(ObjectType::Blob)
            && let Ok(name) = entry.name()
        {
            entries.insert(format!("{dir}{name}"), entry.id());
        }
        TreeWalkResult::Ok
    })?;
    Ok(entries)
}

fn count_changes(before: &BTreeMap<String, Oid>, after: &BTreeMap<String, Oid>) -> ChangeCounts {
    let mut counts = ChangeCounts::default();
    for (path, id) in after {
        match before.get(path) {
            None => counts.added += 1,
            Some(old) if old != id => counts.modified += 1,
            Some(_) => {}
        }
    }
    counts.deleted = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .count() as u32;
    counts
}

fn commit_message(kind: SnapshotKind, summary: &str) -> String {
    let summary = summary.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    let summary = summary.trim();
    let summary = if summary.is_empty() {
        kind.as_str()
    } else {
        summary
    };
    format!("{summary}\n\n{KIND_TRAILER}: {}\n", kind.as_str())
}

/// Summary (first paragraph) and kind trailer of a commit message.
fn parse_message(message: &str) -> (String, Option<SnapshotKind>) {
    let summary = message
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    let prefix = format!("{KIND_TRAILER}:");
    let kind = message
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|value| SnapshotKind::parse(value.trim()));
    (summary, kind)
}

struct Comparison {
    binary: bool,
    too_large: bool,
    /// Lines added and removed, for text files small enough to compare.
    stats: Option<(u32, u32)>,
    hunks: Vec<DiffHunk>,
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_PROBE)].contains(&0)
}

fn compare(
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    with_hunks: bool,
) -> Result<Comparison, HistoryError> {
    let sides = [old, new];
    let binary = sides.iter().flatten().any(|bytes| is_binary(bytes));
    let too_large = sides
        .iter()
        .flatten()
        .any(|bytes| bytes.len() as u64 > MAX_DIFF_BYTES);
    if binary || too_large {
        return Ok(Comparison {
            binary,
            too_large,
            stats: None,
            hunks: Vec::new(),
        });
    }
    let mut options = DiffOptions::new();
    options.context_lines(CONTEXT_LINES);
    let patch = Patch::from_buffers(
        old.unwrap_or_default(),
        None,
        new.unwrap_or_default(),
        None,
        Some(&mut options),
    )?;
    let (_, additions, deletions) = patch.line_stats()?;
    let mut hunks = Vec::new();
    if with_hunks {
        for index in 0..patch.num_hunks() {
            let (hunk, line_count) = patch.hunk(index)?;
            let mut lines = Vec::with_capacity(line_count);
            for line_index in 0..line_count {
                let line = patch.line_in_hunk(index, line_index)?;
                let kind = match line.origin() {
                    ' ' => LineKind::Context,
                    '+' => LineKind::Added,
                    '-' => LineKind::Removed,
                    // End-of-file markers ("\ No newline at end of file").
                    _ => continue,
                };
                let text = String::from_utf8_lossy(line.content());
                let text = text.strip_suffix('\n').unwrap_or(&text);
                let text = text.strip_suffix('\r').unwrap_or(text);
                lines.push(DiffLine {
                    kind,
                    text: text.to_owned(),
                });
            }
            hunks.push(DiffHunk {
                old_start: hunk.old_start(),
                old_lines: hunk.old_lines(),
                new_start: hunk.new_start(),
                new_lines: hunk.new_lines(),
                lines,
            });
        }
    }
    Ok(Comparison {
        binary: false,
        too_large: false,
        stats: Some((
            u32::try_from(additions).unwrap_or(u32::MAX),
            u32::try_from(deletions).unwrap_or(u32::MAX),
        )),
        hunks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u64 = 1024;

    fn request(kind: SnapshotKind) -> SnapshotRequest<'static> {
        SnapshotRequest {
            kind,
            summary: "test",
            force: false,
            max_file_bytes: LIMIT,
            author: None,
        }
    }

    fn put(root: &Path, path: &str, content: &[u8]) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        history: History,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let history = History::open_or_init(&dir.path().join("history.git")).unwrap();
        Fixture {
            _dir: dir,
            root,
            history,
        }
    }

    fn snap(f: &Fixture, kind: SnapshotKind) -> Option<SnapshotCommit> {
        f.history.snapshot(&f.root, request(kind)).unwrap()
    }

    fn content_at(f: &Fixture, rev: &str, path: &str) -> Option<Vec<u8>> {
        let commit = f.history.commit(rev).unwrap();
        let side = f.history.tree_side(Some(&commit)).unwrap();
        side.read(path).unwrap()
    }

    #[test]
    fn recognises_names_git_cannot_store() {
        for name in [".git", ".GIT", "Git~1", ".git.", ".git "] {
            assert!(is_git_name(name), "{name}");
        }
        for name in [".gitignore", "git", ".github", "my.git"] {
            assert!(!is_git_name(name), "{name}");
        }
    }

    #[test]
    fn kinds_round_trip() {
        for kind in SnapshotKind::ALL {
            assert_eq!(SnapshotKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(SnapshotKind::parse("nope"), None);
    }

    #[test]
    fn records_nested_and_binary_files_exactly() {
        let f = fixture();
        put(&f.root, "main.tex", b"\\documentclass{article}\n");
        put(&f.root, "chapters/intro.tex", b"Intro\n");
        let png = [0x89, b'P', b'N', b'G', 0, 1, 2, 255];
        put(&f.root, "figures/plot.png", &png);

        let commit = snap(&f, SnapshotKind::Created).unwrap();
        assert_eq!(
            commit.counts,
            ChangeCounts {
                added: 3,
                modified: 0,
                deleted: 0
            }
        );
        assert_eq!(
            content_at(&f, &commit.rev, "chapters/intro.tex").unwrap(),
            b"Intro\n"
        );
        assert_eq!(
            content_at(&f, &commit.rev, "figures/plot.png").unwrap(),
            png
        );
        assert_eq!(f.history.head().unwrap(), Some(commit.rev));
    }

    #[test]
    fn skips_unchanged_projects_unless_forced() {
        let f = fixture();
        put(&f.root, "main.tex", b"one");
        let first = snap(&f, SnapshotKind::Created).unwrap();
        assert_eq!(snap(&f, SnapshotKind::Compiled), None);

        let forced = f
            .history
            .snapshot(
                &f.root,
                SnapshotRequest {
                    force: true,
                    ..request(SnapshotKind::Manual)
                },
            )
            .unwrap()
            .unwrap();
        assert_ne!(forced.rev, first.rev);
        assert_eq!(forced.counts, ChangeCounts::default());

        put(&f.root, "main.tex", b"two");
        put(&f.root, "new.tex", b"new");
        let next = snap(&f, SnapshotKind::Compiled).unwrap();
        assert_eq!(
            next.counts,
            ChangeCounts {
                added: 1,
                modified: 1,
                deleted: 0
            }
        );
    }

    #[test]
    fn leaves_out_git_dir_temp_files_and_big_files() {
        let f = fixture();
        put(&f.root, "main.tex", b"x");
        put(&f.root, ".git/config", b"[core]");
        put(&f.root, "sub/.git", b"gitdir: elsewhere");
        put(&f.root, &format!("{}tmp", crate::paths::TEMP_PREFIX), b"t");
        put(&f.root, "big.pdf", &vec![b'a'; LIMIT as usize + 1]);
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hostname", f.root.join("link.tex")).unwrap();

        let commit = snap(&f, SnapshotKind::Created).unwrap();
        let tree = tree_entries(&f.history.commit(&commit.rev).unwrap().tree().unwrap()).unwrap();
        let paths: Vec<&str> = tree.keys().map(String::as_str).collect();
        assert_eq!(paths, ["main.tex"]);
    }

    #[test]
    fn the_author_is_whoever_led_to_the_snapshot() {
        let f = fixture();
        put(&f.root, "main.tex", b"x");
        let by_minh = f
            .history
            .snapshot(
                &f.root,
                SnapshotRequest {
                    author: Some("Minh Tran"),
                    ..request(SnapshotKind::Manual)
                },
            )
            .unwrap()
            .unwrap();
        let commit = f.history.commit(&by_minh.rev).unwrap();
        assert_eq!(commit.author().name().ok(), Some("Minh Tran"));
        assert_eq!(commit.committer().name().ok(), Some(AUTHOR_NAME));

        put(&f.root, "main.tex", b"y");
        let unsigned = snap(&f, SnapshotKind::Compiled).unwrap();
        let commit = f.history.commit(&unsigned.rev).unwrap();
        assert_eq!(commit.author().name().ok(), Some(AUTHOR_NAME));
    }

    #[test]
    fn commit_message_carries_summary_and_kind() {
        let f = fixture();
        put(&f.root, "main.tex", b"x");
        let commit = f
            .history
            .snapshot(
                &f.root,
                SnapshotRequest {
                    summary: "Before AI edit of\nmain.tex",
                    ..request(SnapshotKind::BeforeAiEdit)
                },
            )
            .unwrap()
            .unwrap();
        let message = f
            .history
            .commit(&commit.rev)
            .unwrap()
            .message()
            .unwrap()
            .to_owned();
        assert_eq!(
            message,
            "Before AI edit of main.tex\n\nOpen-Prism-Kind: beforeAiEdit\n"
        );
        let info = f.history.unrecorded(|_| false).unwrap();
        assert_eq!(info[0].kind, Some(SnapshotKind::BeforeAiEdit));
        assert_eq!(info[0].summary, "Before AI edit of main.tex");
    }

    fn two_snapshots(f: &Fixture) -> (String, String) {
        put(&f.root, "main.tex", b"a\nb\nc\n");
        put(&f.root, "intro.tex", b"Hello\n");
        put(&f.root, "old.tex", b"gone soon\n");
        let a = snap(f, SnapshotKind::Created).unwrap().rev;
        put(&f.root, "intro.tex", b"Hello world\n");
        put(&f.root, "refs.bib", b"@book{x}\n");
        std::fs::remove_file(f.root.join("old.tex")).unwrap();
        let b = snap(f, SnapshotKind::Compiled).unwrap().rev;
        (a, b)
    }

    #[test]
    fn lists_changes_against_parent_other_snapshot_and_empty() {
        let f = fixture();
        let (a, b) = two_snapshots(&f);

        let changes = f
            .history
            .changes(&b, Against::Parent, &f.root, LIMIT)
            .unwrap();
        let summary: Vec<(&str, ChangeStatus, Option<u32>, Option<u32>)> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.status, c.additions, c.deletions))
            .collect();
        assert_eq!(
            summary,
            [
                ("intro.tex", ChangeStatus::Modified, Some(1), Some(1)),
                ("old.tex", ChangeStatus::Deleted, Some(0), Some(1)),
                ("refs.bib", ChangeStatus::Added, Some(1), Some(0)),
            ]
        );
        assert_eq!(
            f.history
                .changes(&b, Against::Rev(&a), &f.root, LIMIT)
                .unwrap(),
            changes
        );

        // The first snapshot is compared with an empty project.
        let first = f
            .history
            .changes(&a, Against::Parent, &f.root, LIMIT)
            .unwrap();
        assert!(first.iter().all(|c| c.status == ChangeStatus::Added));
        assert_eq!(first.len(), 3);
    }

    #[test]
    fn compares_with_the_current_files_without_writing_objects() {
        let f = fixture();
        let (a, _) = two_snapshots(&f);
        put(&f.root, "main.tex", b"a\nB\nc\n");
        let objects = f.history.object_count();

        let changes = f
            .history
            .changes(&a, Against::Current, &f.root, LIMIT)
            .unwrap();
        let paths: Vec<(&str, ChangeStatus)> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.status))
            .collect();
        // From the current files to snapshot A.
        assert_eq!(
            paths,
            [
                ("intro.tex", ChangeStatus::Modified),
                ("main.tex", ChangeStatus::Modified),
                ("old.tex", ChangeStatus::Added),
                ("refs.bib", ChangeStatus::Deleted),
            ]
        );
        assert_eq!(f.history.object_count(), objects);
    }

    #[test]
    fn file_diff_has_hunks_with_context() {
        let f = fixture();
        let lines: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        put(&f.root, "main.tex", lines.as_bytes());
        snap(&f, SnapshotKind::Created).unwrap();
        put(
            &f.root,
            "main.tex",
            lines.replace("line 10\n", "line ten\n").as_bytes(),
        );
        let rev = snap(&f, SnapshotKind::Compiled).unwrap().rev;

        let diff = f
            .history
            .file_diff(
                &rev,
                Against::Parent,
                &RelPath::parse("main.tex").unwrap(),
                &f.root,
                LIMIT,
            )
            .unwrap();
        assert!(!diff.binary && !diff.too_large);
        assert_eq!(diff.hunks.len(), 1);
        let hunk = &diff.hunks[0];
        assert_eq!((hunk.old_start, hunk.old_lines), (7, 7));
        assert_eq!((hunk.new_start, hunk.new_lines), (7, 7));
        let shown: Vec<(LineKind, &str)> = hunk
            .lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect();
        assert_eq!(shown[3], (LineKind::Removed, "line 10"));
        assert_eq!(shown[4], (LineKind::Added, "line ten"));
        assert_eq!(shown.len(), 8);

        let missing = f.history.file_diff(
            &rev,
            Against::Parent,
            &RelPath::parse("nope.tex").unwrap(),
            &f.root,
            LIMIT,
        );
        assert!(matches!(missing, Err(HistoryError::NoSuchFile(_))));
    }

    #[test]
    fn binary_and_large_files_have_no_line_diff() {
        let f = fixture();
        put(&f.root, "plot.png", &[1, 0, 2]);
        put(&f.root, "data.txt", b"small\n");
        snap(&f, SnapshotKind::Created).unwrap();
        put(&f.root, "plot.png", &[1, 0, 3, 4]);
        let large = vec![b'x'; MAX_DIFF_BYTES as usize + 1];
        put(&f.root, "data.txt", &large);
        let rev = f
            .history
            .snapshot(
                &f.root,
                SnapshotRequest {
                    max_file_bytes: MAX_FILE_BYTES,
                    ..request(SnapshotKind::Compiled)
                },
            )
            .unwrap()
            .unwrap()
            .rev;

        let changes = f
            .history
            .changes(&rev, Against::Parent, &f.root, MAX_FILE_BYTES)
            .unwrap();
        let png = changes.iter().find(|c| c.path == "plot.png").unwrap();
        assert!(png.binary);
        assert_eq!(
            (png.additions, png.old_size, png.new_size),
            (None, Some(3), Some(4))
        );

        let diff = f
            .history
            .file_diff(
                &rev,
                Against::Parent,
                &RelPath::parse("data.txt").unwrap(),
                &f.root,
                MAX_FILE_BYTES,
            )
            .unwrap();
        assert!(diff.too_large && !diff.binary && diff.hunks.is_empty());
        assert_eq!(diff.new_size, Some(large.len() as u64));
    }

    #[test]
    fn restores_one_file() {
        let f = fixture();
        let (a, _) = two_snapshots(&f);
        let intro = RelPath::parse("intro.tex").unwrap();
        let changed = f
            .history
            .restore(&a, &f.root, Some(std::slice::from_ref(&intro)), LIMIT)
            .unwrap();
        assert_eq!(changed, ["intro.tex"]);
        assert_eq!(std::fs::read(f.root.join("intro.tex")).unwrap(), b"Hello\n");
        // Other files are left alone.
        assert!(f.root.join("refs.bib").exists());
        assert!(!f.root.join("old.tex").exists());

        // A file absent from the snapshot is deleted.
        let refs = RelPath::parse("refs.bib").unwrap();
        let changed = f
            .history
            .restore(&a, &f.root, Some(std::slice::from_ref(&refs)), LIMIT)
            .unwrap();
        assert_eq!(changed, ["refs.bib"]);
        assert!(!f.root.join("refs.bib").exists());
    }

    #[test]
    fn restores_the_whole_project_but_not_unrecorded_files() {
        let f = fixture();
        let (a, _) = two_snapshots(&f);
        put(&f.root, "main.tex", b"changed\n");
        put(&f.root, ".git/HEAD", b"ref: refs/heads/main\n");
        let big = vec![b'b'; LIMIT as usize + 1];
        put(&f.root, "old.tex", &big);
        #[cfg(unix)]
        std::os::unix::fs::symlink("main.tex", f.root.join("alias.tex")).unwrap();

        let changed = f.history.restore(&a, &f.root, None, LIMIT).unwrap();
        // `old.tex` is too big to be recorded now: restoring must not overwrite it.
        assert_eq!(changed, ["intro.tex", "main.tex", "refs.bib"]);
        assert_eq!(
            std::fs::read(f.root.join("main.tex")).unwrap(),
            b"a\nb\nc\n"
        );
        assert_eq!(std::fs::read(f.root.join("intro.tex")).unwrap(), b"Hello\n");
        assert!(!f.root.join("refs.bib").exists());
        assert_eq!(std::fs::read(f.root.join("old.tex")).unwrap(), big);
        assert!(f.root.join(".git/HEAD").exists());
        #[cfg(unix)]
        assert!(f.root.join("alias.tex").is_symlink());
    }

    #[test]
    fn restore_brings_back_deleted_files() {
        let f = fixture();
        let (a, _) = two_snapshots(&f);
        let changed = f.history.restore(&a, &f.root, None, LIMIT).unwrap();
        assert_eq!(changed, ["intro.tex", "old.tex", "refs.bib"]);
        assert_eq!(
            std::fs::read(f.root.join("old.tex")).unwrap(),
            b"gone soon\n"
        );
        // Restoring the same snapshot again changes nothing.
        assert!(
            f.history
                .restore(&a, &f.root, None, LIMIT)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_paths_outside_the_project_and_unknown_revisions() {
        let f = fixture();
        let (a, _) = two_snapshots(&f);
        assert!(RelPath::parse("../x").is_err());
        assert!(matches!(
            f.history.restore(&"0".repeat(40), &f.root, None, LIMIT),
            Err(HistoryError::UnknownRevision(_))
        ));
        assert!(matches!(
            f.history
                .changes("not-a-rev", Against::Parent, &f.root, LIMIT),
            Err(HistoryError::UnknownRevision(_))
        ));
        assert!(matches!(
            f.history.changes(&a, Against::Rev("abc"), &f.root, LIMIT),
            Err(HistoryError::UnknownRevision(_))
        ));
    }

    #[test]
    fn unrecorded_stops_at_the_first_known_snapshot() {
        let f = fixture();
        let (a, b) = two_snapshots(&f);
        put(&f.root, "main.tex", b"third\n");
        let c = snap(&f, SnapshotKind::Manual).unwrap().rev;

        let missing = f.history.unrecorded(|rev| rev == a).unwrap();
        let revs: Vec<&str> = missing.iter().map(|info| info.rev.as_str()).collect();
        assert_eq!(revs, [c.as_str(), b.as_str()]);
        assert_eq!(missing[0].kind, Some(SnapshotKind::Manual));
        assert_eq!(missing[0].counts.modified, 1);
        assert!(missing[0].time_ms > 0);

        assert!(f.history.unrecorded(|rev| rev == c).unwrap().is_empty());
    }

    #[test]
    fn opening_a_missing_repository_gives_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            History::open(&dir.path().join("none.git"))
                .unwrap()
                .is_none()
        );
        let history = History::open_or_init(&dir.path().join("new.git")).unwrap();
        assert_eq!(history.head().unwrap(), None);
    }
}
