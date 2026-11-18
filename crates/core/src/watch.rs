//! Watching project directories for changes made outside Open Prism (another editor, `git
//! pull`, …) as well as our own writes. Events are grouped per project and debounced.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::paths::{RelPath, TEMP_PREFIX};

/// Changed paths of one project, relative to its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectChanges {
    pub project_id: String,
    pub paths: BTreeSet<String>,
}

/// Keeps the OS watcher alive; dropping it stops watching.
pub struct ProjectsWatcher {
    _watcher: RecommendedWatcher,
}

/// Watches `projects_dir/<project id>/**` and sends batches of changes, at most one batch per
/// `debounce` interval.
pub fn watch_projects(
    projects_dir: &Path,
    debounce: Duration,
) -> notify::Result<(ProjectsWatcher, mpsc::Receiver<Vec<ProjectChanges>>)> {
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        for path in event.paths {
            let _ = raw_tx.send(path);
        }
    })?;
    watcher.watch(projects_dir, RecursiveMode::Recursive)?;

    let root = projects_dir.to_path_buf();
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(first) = raw_rx.recv().await {
            let mut pending: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            record(&root, &first, &mut pending);
            let deadline = tokio::time::sleep(debounce);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    () = &mut deadline => break,
                    next = raw_rx.recv() => match next {
                        Some(path) => record(&root, &path, &mut pending),
                        None => break,
                    },
                }
            }
            if pending.is_empty() {
                continue;
            }
            let batch = pending
                .into_iter()
                .map(|(project_id, paths)| ProjectChanges { project_id, paths })
                .collect();
            if tx.send(batch).await.is_err() {
                return;
            }
        }
    });
    Ok((ProjectsWatcher { _watcher: watcher }, rx))
}

fn record(root: &Path, path: &Path, pending: &mut BTreeMap<String, BTreeSet<String>>) {
    let Ok(relative) = path.strip_prefix(root) else {
        return;
    };
    let mut components = relative.components();
    let Some(project_id) = components.next().and_then(|c| c.as_os_str().to_str()) else {
        return;
    };
    let inner = components.as_path();
    if inner
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(TEMP_PREFIX))
    {
        return;
    }
    let entry = pending.entry(project_id.to_owned()).or_default();
    // The project directory itself changed (created/deleted): report it as "".
    if inner.as_os_str().is_empty() {
        entry.insert(String::new());
    } else if let Some(rel) = RelPath::from_path(Path::new(""), inner) {
        entry.insert(rel.as_str().to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_changes_per_project_and_skips_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(projects.join("p1/chapters")).unwrap();
        std::fs::create_dir_all(projects.join("p2")).unwrap();
        let (_watcher, mut rx) = watch_projects(&projects, Duration::from_millis(100)).unwrap();

        std::fs::write(projects.join("p1/chapters/intro.tex"), "x").unwrap();
        std::fs::write(projects.join(format!("p1/{TEMP_PREFIX}tmp")), "x").unwrap();
        std::fs::write(projects.join("p2/main.tex"), "y").unwrap();

        let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while seen.len() < 2 {
            let batch = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("no change reported in time")
                .unwrap();
            for changes in batch {
                seen.entry(changes.project_id)
                    .or_default()
                    .extend(changes.paths);
            }
        }
        assert!(seen["p1"].contains("chapters/intro.tex"));
        assert!(seen["p1"].iter().all(|p| !p.contains(TEMP_PREFIX)));
        assert!(seen["p2"].contains("main.tex"));
    }
}
