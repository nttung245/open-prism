//! The jobs store on a real (temporary) database.

use serde_json::json;

use super::store::{self, JobState};
use crate::auth::AccessToken;
use crate::db::now_ms;
use crate::state::AppState;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

async fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path(), AccessToken::new(TOKEN).unwrap(), Vec::new())
        .await
        .unwrap();
    (dir, state)
}

async fn finish_at(state: &AppState, id: &str, finished_at: i64) {
    sqlx::query("UPDATE jobs SET state = 'succeeded', finished_at = ? WHERE id = ?")
        .bind(finished_at)
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn lists_newest_first_without_old_finished_jobs() {
    let (_dir, state) = state().await;
    let a = store::create(&state.db, "fake", "A", &json!({}))
        .await
        .unwrap();
    let b = store::create(&state.db, "fake", "B", &json!({}))
        .await
        .unwrap();
    let c = store::create(&state.db, "fake", "C", &json!({"n": 1}))
        .await
        .unwrap();
    assert_eq!(a.state, JobState::Queued);
    assert_eq!(c.steps_total, None);

    let now = now_ms();
    finish_at(&state, &a.id, now - store::LIST_FINISHED_MS - 1000).await;
    finish_at(&state, &b.id, now - 1000).await;

    let titles: Vec<_> = store::list(&state.db, now)
        .await
        .unwrap()
        .into_iter()
        .map(|job| job.title)
        .collect();
    assert_eq!(titles, ["C", "B"]);
    assert_eq!(
        store::get(&state.db, &c.id).await.unwrap().unwrap().params,
        json!({"n": 1})
    );
}

#[tokio::test]
async fn a_step_counts_once() {
    let (_dir, state) = state().await;
    let job = store::create(&state.db, "fake", "J", &json!({}))
        .await
        .unwrap();
    store::plan(&state.db, &job.id, 3).await.unwrap();
    assert_eq!(
        store::pending_steps(&state.db, &job.id).await.unwrap(),
        [0, 1, 2]
    );

    assert!(store::step_done(&state.db, &job.id, 1).await.unwrap());
    assert!(!store::step_done(&state.db, &job.id, 1).await.unwrap());
    let view = store::view(&state.db, &job.id).await.unwrap().unwrap();
    assert_eq!((view.steps_done, view.steps_total), (1, Some(3)));
    assert_eq!(
        store::pending_steps(&state.db, &job.id).await.unwrap(),
        [0, 2]
    );
    assert_eq!(store::add_attempt(&state.db, &job.id, 0).await.unwrap(), 1);
    assert_eq!(store::add_attempt(&state.db, &job.id, 0).await.unwrap(), 2);
}

#[tokio::test]
async fn states_move_only_the_allowed_way() {
    let (_dir, state) = state().await;
    let job = store::create(&state.db, "fake", "J", &json!({}))
        .await
        .unwrap();
    assert!(
        !store::retry(&state.db, &job.id).await.unwrap(),
        "only a failed job retries"
    );
    assert!(store::mark_running(&state.db, &job.id).await.unwrap());
    assert!(!store::mark_running(&state.db, &job.id).await.unwrap());

    store::plan(&state.db, &job.id, 2).await.unwrap();
    store::add_attempt(&state.db, &job.id, 1).await.unwrap();
    assert!(store::fail(&state.db, &job.id, "broken").await.unwrap());
    let failed = store::view(&state.db, &job.id).await.unwrap().unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.error.as_deref(), Some("broken"));
    assert!(failed.finished_at.is_some());
    assert!(
        !store::mark_cancelled(&state.db, &job.id).await.unwrap(),
        "already finished"
    );

    assert!(store::retry(&state.db, &job.id).await.unwrap());
    let queued = store::view(&state.db, &job.id).await.unwrap().unwrap();
    assert_eq!(
        (queued.state, queued.error, queued.finished_at),
        (JobState::Queued, None, None)
    );
    // Attempts start again.
    assert_eq!(store::add_attempt(&state.db, &job.id, 1).await.unwrap(), 1);

    assert!(store::mark_running(&state.db, &job.id).await.unwrap());
    assert_eq!(store::requeue_running(&state.db).await.unwrap(), 1);
    assert!(store::mark_cancelled(&state.db, &job.id).await.unwrap());
    assert!(!store::succeed(&state.db, &job.id, Some("p")).await.unwrap());
}

#[tokio::test]
async fn deleting_a_job_deletes_its_steps() {
    let (_dir, state) = state().await;
    let job = store::create(&state.db, "fake", "J", &json!({}))
        .await
        .unwrap();
    store::plan(&state.db, &job.id, 4).await.unwrap();
    assert!(
        !store::delete_finished(&state.db, &job.id).await.unwrap(),
        "not finished"
    );
    finish_at(&state, &job.id, now_ms()).await;
    assert!(store::delete_finished(&state.db, &job.id).await.unwrap());
    let steps: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_steps")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(steps, 0);
    assert!(store::view(&state.db, &job.id).await.unwrap().is_none());
}

#[tokio::test]
async fn cleanup_deletes_old_jobs_and_their_files() {
    let (_dir, state) = state().await;
    let old = store::create(&state.db, "fake", "old", &json!({}))
        .await
        .unwrap();
    let recent = store::create(&state.db, "fake", "recent", &json!({}))
        .await
        .unwrap();
    for job in [&old, &recent] {
        std::fs::create_dir_all(state.paths.job_dir(&job.id)).unwrap();
        std::fs::write(state.paths.job_dir(&job.id).join("page.tex"), "x").unwrap();
    }
    finish_at(&state, &old.id, now_ms() - store::KEEP_FINISHED_MS - 1000).await;
    finish_at(&state, &recent.id, now_ms() - 1000).await;

    assert_eq!(super::cleanup(&state).await.unwrap(), 1);
    assert!(!state.paths.job_dir(&old.id).exists());
    assert!(state.paths.job_dir(&recent.id).join("page.tex").exists());
    assert!(store::view(&state.db, &old.id).await.unwrap().is_none());
    assert!(store::view(&state.db, &recent.id).await.unwrap().is_some());
}
