use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use openprism_core::engines::EngineInfo;
use sqlx::SqlitePool;

use crate::auth::AccessToken;
use crate::chat::RunningTurns;
use crate::compiler::Compiler;
use crate::events::EventHub;
use crate::jobs::Jobs;
use crate::login_limit::LoginLimiter;
use crate::mode::Mode;
use crate::synctex_cache::SyncTexCache;

/// Shared, read-mostly application state.
#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
pub struct Inner {
    pub token: AccessToken,
    /// Single-user (the default) or server mode.
    pub mode: Mode,
    /// Failed sign-ins in server mode.
    pub login_limit: LoginLimiter,
    /// TeX toolchains found at startup.
    pub engines: Vec<EngineInfo>,
    pub paths: DataPaths,
    pub db: SqlitePool,
    pub events: EventHub,
    pub compiler: Compiler,
    pub synctex: SyncTexCache,
    pub running_turns: RunningTurns,
    /// Background jobs: the kinds the app knows and the job running now.
    pub jobs: Jobs,
    /// One lock per project so a version check and the write that follows are atomic.
    write_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Layout of the data directory.
#[derive(Debug, Clone)]
pub struct DataPaths {
    pub root: PathBuf,
    pub database: PathBuf,
    /// `projects/<id>/`: the user's files, nothing else.
    pub projects: PathBuf,
    /// `build/<id>/`: latexmk output directory, kept for incremental builds.
    pub build: PathBuf,
    /// `runs/<id>/<run>/`: outputs of finished compilations.
    pub runs: PathBuf,
    /// `cache/tex/`: TeX caches mounted into the container.
    pub tex_cache: PathBuf,
    /// `tmp/`: uploads in progress.
    pub tmp: PathBuf,
    /// `history/<id>.git`: snapshots of each project, outside the project directory.
    pub history: PathBuf,
    /// `attachments/<project>/<thread>/`: images sent in AI conversations.
    pub attachments: PathBuf,
    /// `jobs/<id>/`: working files of background jobs, outside `projects/` so the file watcher
    /// never sees them.
    pub jobs: PathBuf,
}

impl DataPaths {
    /// Creates the directories and resolves `root` to an absolute path without symlinks, so
    /// the paths can be mounted into containers and compared with paths TeX reports.
    pub fn prepare(root: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("cannot create data dir {}", root.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", root.display()))?;
        let paths = Self {
            database: root.join("open-prism.db"),
            projects: root.join("projects"),
            build: root.join("build"),
            runs: root.join("runs"),
            tex_cache: root.join("cache").join("tex"),
            tmp: root.join("tmp"),
            history: root.join("history"),
            attachments: root.join("attachments"),
            jobs: root.join("jobs"),
            root,
        };
        for dir in [
            &paths.projects,
            &paths.build,
            &paths.runs,
            &paths.tex_cache,
            &paths.tmp,
            &paths.history,
            &paths.attachments,
            &paths.jobs,
        ] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        Ok(paths)
    }

    pub fn project_dir(&self, id: &str) -> PathBuf {
        self.projects.join(id)
    }

    pub fn build_dir(&self, id: &str) -> PathBuf {
        self.build.join(id)
    }

    pub fn runs_dir(&self, id: &str) -> PathBuf {
        self.runs.join(id)
    }

    pub fn run_dir(&self, id: &str, run_id: &str) -> PathBuf {
        self.runs.join(id).join(run_id)
    }

    /// Working files of one background job.
    pub fn job_dir(&self, id: &str) -> PathBuf {
        self.jobs.join(id)
    }

    pub fn history_dir(&self, id: &str) -> PathBuf {
        self.history.join(format!("{id}.git"))
    }

    /// Images of a project's AI conversations.
    pub fn attachments_dir(&self, project_id: &str) -> PathBuf {
        self.attachments.join(project_id)
    }

    /// Images of one AI conversation.
    pub fn thread_attachments_dir(&self, project_id: &str, thread_id: &str) -> PathBuf {
        self.attachments_dir(project_id).join(thread_id)
    }
}

impl AppState {
    /// Opens the state in single-user mode.
    #[cfg(test)]
    pub async fn open(
        data_dir: &Path,
        token: AccessToken,
        engines: Vec<EngineInfo>,
    ) -> anyhow::Result<Self> {
        Self::open_in(Mode::Single, data_dir, token, engines).await
    }

    pub async fn open_in(
        mode: Mode,
        data_dir: &Path,
        token: AccessToken,
        engines: Vec<EngineInfo>,
    ) -> anyhow::Result<Self> {
        let paths = DataPaths::prepare(data_dir)?;
        let db = crate::db::open(&paths.database).await?;
        let state = Self(Arc::new(Inner {
            token,
            mode,
            login_limit: LoginLimiter::default(),
            compiler: Compiler::new(engines.clone()),
            engines,
            paths,
            db,
            events: EventHub::default(),
            synctex: SyncTexCache::default(),
            running_turns: RunningTurns::default(),
            jobs: Jobs::default(),
            write_locks: Mutex::default(),
        }));
        state
            .jobs
            .register(Arc::new(crate::jobs::pdf_import::PdfImport));
        Ok(state)
    }
}

impl Inner {
    /// Serialises writes to one project.
    pub async fn lock_project(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .write_locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            locks.entry(id.to_owned()).or_default().clone()
        };
        lock.lock_owned().await
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_creates_the_history_directory() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let paths = DataPaths::prepare(dir.path())?;
        assert!(paths.history.is_dir());
        assert_eq!(paths.history_dir("p1"), paths.history.join("p1.git"));
        Ok(())
    }
}
