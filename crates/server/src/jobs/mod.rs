//! Background jobs: long work in steps that survives a restart and leaves room for the user's
//! own AI requests (see the `background-jobs` spec).

mod gate;
pub mod pdf_import;
mod scheduler;
pub mod store;
pub use gate::AiAnswer;
#[cfg(test)]
pub use scheduler::create;
pub use scheduler::{
    JobContext, JobError, JobKind, Jobs, cancel, create_prepared, delete, retry, run,
};

use crate::db::now_ms;
use crate::state::AppState;

/// Deletes jobs that finished more than 30 days ago, with their files.
pub async fn cleanup(state: &AppState) -> Result<usize, sqlx::Error> {
    let ids = store::delete_finished_before(&state.db, now_ms() - store::KEEP_FINISHED_MS).await?;
    for id in &ids {
        remove_dir(&state.paths.job_dir(id)).await;
    }
    Ok(ids.len())
}

/// Removes a job's working directory; a missing one is fine.
async fn remove_dir(dir: &std::path::Path) {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(%err, dir = %dir.display(), "cannot remove a job's files"),
    }
}

#[cfg(test)]
mod gate_tests;
#[cfg(test)]
pub(crate) mod scheduler_tests;
#[cfg(test)]
mod store_tests;
