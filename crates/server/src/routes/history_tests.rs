//! End-to-end tests of the history API against a real data directory.

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use openprism_core::history::{History, MAX_FILE_BYTES, SnapshotKind, SnapshotRequest};
use serde_json::{Value, json};

use super::api_tests::TestApp;
use crate::events::ProjectEvent;

async fn put(app: &TestApp, id: &str, path: &str, content: &str) {
    let request = TestApp::request(Method::PUT, &format!("/api/projects/{id}/files/{path}"))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(content.to_owned()))
        .unwrap();
    assert_eq!(app.send(request).await.status, StatusCode::OK);
}

async fn read(app: &TestApp, id: &str, path: &str) -> Option<String> {
    let reply = app
        .call(
            Method::GET,
            &format!("/api/projects/{id}/files/{path}"),
            None,
        )
        .await;
    match reply.status {
        StatusCode::OK => Some(String::from_utf8(reply.body.to_vec()).unwrap()),
        StatusCode::NOT_FOUND => None,
        other => panic!("unexpected status {other}"),
    }
}

async fn delete(app: &TestApp, id: &str, path: &str) {
    let reply = app
        .call(
            Method::POST,
            &format!("/api/projects/{id}/file-ops"),
            Some(json!({ "op": "delete", "path": path })),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
}

async fn save(app: &TestApp, id: &str, label: &str) -> Value {
    let reply = app
        .call(
            Method::POST,
            &format!("/api/projects/{id}/history"),
            Some(json!({ "label": label })),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.json());
    reply.json()
}

async fn timeline(app: &TestApp, id: &str) -> Vec<Value> {
    let reply = app
        .call(
            Method::GET,
            &format!("/api/projects/{id}/history?limit=200"),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    reply.json()["snapshots"].as_array().unwrap().clone()
}

fn rev(snapshot: &Value) -> String {
    snapshot["rev"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn manual_snapshots_labels_and_pages() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    let history = format!("/api/projects/{id}/history");

    put(&app, &id, "main.tex", "v1").await;
    let first = save(&app, &id, "  First ").await;
    assert_eq!(first["label"], "First");
    assert_eq!(first["kind"], "manual");
    assert_eq!(rev(&first).len(), 40);

    for label in [String::new(), "   ".to_owned(), "x".repeat(101)] {
        let reply = app
            .call(Method::POST, &history, Some(json!({ "label": label })))
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }

    put(&app, &id, "main.tex", "v2").await;
    let second = save(&app, &id, "Second").await;
    assert_eq!(second["modified"], 1);
    // A manual snapshot is taken even when nothing changed.
    let third = save(&app, &id, "Third").await;
    assert_eq!(
        (third["added"].clone(), third["modified"].clone()),
        (json!(0), json!(0))
    );

    let all = timeline(&app, &id).await;
    let labels: Vec<&Value> = all.iter().map(|s| &s["label"]).take(3).collect();
    assert_eq!(labels, [&json!("Third"), &json!("Second"), &json!("First")]);

    let page = app
        .call(Method::GET, &format!("{history}?limit=2"), None)
        .await
        .json();
    assert_eq!(page["snapshots"].as_array().unwrap().len(), 2);
    assert_eq!(page["nextBefore"], second["rev"]);
    let next = app
        .call(
            Method::GET,
            &format!("{history}?limit=2&before={}", rev(&second)),
            None,
        )
        .await
        .json();
    assert_eq!(next["snapshots"][0]["rev"], first["rev"]);

    let uri = format!("{history}/{}", rev(&first));
    let renamed = app
        .call(Method::PATCH, &uri, Some(json!({ "label": "Renamed" })))
        .await;
    assert_eq!(renamed.json()["label"], "Renamed");
    assert_eq!(renamed.json()["createdAt"], first["createdAt"]);
    let cleared = app
        .call(Method::PATCH, &uri, Some(json!({ "label": null })))
        .await;
    assert_eq!(cleared.json()["label"], Value::Null);
    let bad = app
        .call(Method::PATCH, &uri, Some(json!({ "label": "" })))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn commits_without_metadata_are_picked_up() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    put(&app, &id, "main.tex", "changed").await;

    // As if the server stopped between the commit and the insert.
    let commit = History::open_or_init(&app.state.paths.history_dir(&id))
        .unwrap()
        .snapshot(
            &app.state.paths.project_dir(&id),
            SnapshotRequest {
                kind: SnapshotKind::Compiled,
                summary: "Compiled successfully",
                force: false,
                max_file_bytes: MAX_FILE_BYTES,
                author: None,
            },
        )
        .unwrap()
        .unwrap();

    let all = timeline(&app, &id).await;
    assert_eq!(all[0]["rev"], commit.rev.as_str());
    assert_eq!(all[0]["kind"], "compiled");
    assert_eq!(all[0]["summary"], "Compiled successfully");
}

#[tokio::test]
async fn new_snapshots_and_labels_are_announced() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    let mut events = app.state.events.subscribe(&id);

    let snapshot = save(&app, &id, "Draft").await;
    assert!(matches!(
        events.recv().await.unwrap(),
        ProjectEvent::HistoryChanged
    ));
    app.call(
        Method::PATCH,
        &format!("/api/projects/{id}/history/{}", rev(&snapshot)),
        Some(json!({ "label": "Final" })),
    )
    .await;
    assert!(matches!(
        events.recv().await.unwrap(),
        ProjectEvent::HistoryChanged
    ));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn diffs_between_snapshots_and_with_the_current_files() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    put(&app, &id, "intro.tex", "Hello\n").await;
    let a = rev(&save(&app, &id, "A").await);
    put(&app, &id, "intro.tex", "Hello world\n").await;
    put(&app, &id, "refs.bib", "@book{x}\n").await;
    let b = rev(&save(&app, &id, "B").await);
    let base = format!("/api/projects/{id}/history");

    let files = app
        .call(Method::GET, &format!("{base}/{b}/diff"), None)
        .await
        .json()["files"]
        .clone();
    assert_eq!(
        files,
        json!([
            { "path": "intro.tex", "status": "modified", "additions": 1, "deletions": 1,
              "binary": false, "oldSize": 6, "newSize": 12 },
            { "path": "refs.bib", "status": "added", "additions": 1, "deletions": 0,
              "binary": false, "oldSize": null, "newSize": 9 },
        ])
    );
    let explicit = app
        .call(Method::GET, &format!("{base}/{b}/diff?against={a}"), None)
        .await
        .json()["files"]
        .clone();
    assert_eq!(explicit, files);

    put(&app, &id, "intro.tex", "Changed again\n").await;
    let current = app
        .call(
            Method::GET,
            &format!("{base}/{a}/diff?against=current"),
            None,
        )
        .await
        .json();
    let paths: Vec<(&str, &str)> = current["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap(), f["status"].as_str().unwrap()))
        .collect();
    assert_eq!(paths, [("intro.tex", "modified"), ("refs.bib", "deleted")]);

    let diff = app
        .call(
            Method::GET,
            &format!("{base}/{b}/diff?path=intro.tex"),
            None,
        )
        .await
        .json();
    assert_eq!(
        diff["hunks"][0]["lines"],
        json!([
            { "kind": "removed", "text": "Hello" },
            { "kind": "added", "text": "Hello world" },
        ])
    );
    let missing = app
        .call(Method::GET, &format!("{base}/{b}/diff?path=nope.tex"), None)
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restores_files_and_the_project_and_can_undo() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    put(&app, &id, "main.tex", "main v1\n").await;
    put(&app, &id, "intro.tex", "intro v1\n").await;
    put(&app, &id, "old.tex", "old\n").await;
    let a = rev(&save(&app, &id, "A").await);
    put(&app, &id, "main.tex", "main v2\n").await;
    put(&app, &id, "intro.tex", "intro v2\n").await;
    delete(&app, &id, "old.tex").await;
    put(&app, &id, "new.tex", "new\n").await;
    let base = format!("/api/projects/{id}/history");

    // One file.
    let reply = app
        .call(
            Method::POST,
            &format!("{base}/{a}/restore"),
            Some(json!({ "paths": ["intro.tex"] })),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.json());
    assert_eq!(reply.json()["changed"], json!(["intro.tex"]));
    assert_eq!(reply.json()["snapshot"]["kind"], "restored");
    assert_eq!(
        reply.json()["snapshot"]["summary"],
        "Restored intro.tex from \u{201c}A\u{201d}"
    );
    assert_eq!(read(&app, &id, "intro.tex").await.unwrap(), "intro v1\n");
    assert_eq!(read(&app, &id, "main.tex").await.unwrap(), "main v2\n");

    let kinds: Vec<String> = timeline(&app, &id)
        .await
        .iter()
        .take(2)
        .map(|s| s["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, ["restored", "beforeRestore"]);

    // The whole project.
    let reply = app
        .call(Method::POST, &format!("{base}/{a}/restore"), None)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.json());
    assert_eq!(
        reply.json()["changed"],
        json!(["main.tex", "new.tex", "old.tex"])
    );
    assert_eq!(read(&app, &id, "main.tex").await.unwrap(), "main v1\n");
    assert_eq!(read(&app, &id, "old.tex").await.unwrap(), "old\n");
    assert_eq!(read(&app, &id, "new.tex").await, None);

    // Undo: restore the snapshot taken just before.
    let before = timeline(&app, &id)
        .await
        .into_iter()
        .find(|s| s["kind"] == "beforeRestore")
        .unwrap();
    let reply = app
        .call(
            Method::POST,
            &format!("{base}/{}/restore", rev(&before)),
            Some(json!({})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(read(&app, &id, "main.tex").await.unwrap(), "main v2\n");
    assert_eq!(read(&app, &id, "new.tex").await.unwrap(), "new\n");
    assert_eq!(read(&app, &id, "old.tex").await, None);
}

#[tokio::test]
async fn rejects_foreign_revisions_and_bad_paths() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    let other = app.create_project("Other").await;
    put(&app, &other, "main.tex", "other\n").await;
    let foreign = rev(&save(&app, &other, "Elsewhere").await);
    let own = rev(&save(&app, &id, "Here").await);
    let base = format!("/api/projects/{id}/history");

    for uri in [
        format!("{base}/{foreign}/diff"),
        format!("{base}/{own}/diff?against={foreign}"),
        format!("{base}/{}/diff", "0".repeat(40)),
        format!("{base}/not-a-rev/diff"),
    ] {
        let reply = app.call(Method::GET, &uri, None).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let reply = app
        .call(Method::POST, &format!("{base}/{foreign}/restore"), None)
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    let before = read(&app, &id, "main.tex").await;
    for paths in [
        json!(["../other/main.tex"]),
        json!(["/etc/passwd"]),
        json!([]),
    ] {
        let reply = app
            .call(
                Method::POST,
                &format!("{base}/{own}/restore"),
                Some(json!({ "paths": paths })),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{paths}");
    }
    assert_eq!(read(&app, &id, "main.tex").await, before);
    let reply = app
        .call(Method::GET, &format!("{base}/{own}/diff?path=../x"), None)
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deleting_a_project_deletes_its_history() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    save(&app, &id, "Keep").await;
    let git_dir = app.state.paths.history_dir(&id);
    assert!(git_dir.is_dir());

    let reply = app
        .call(Method::DELETE, &format!("/api/projects/{id}"), None)
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(!git_dir.exists());
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM snapshots WHERE project_id = ?")
        .bind(&id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

fn compile_result(status: &str) -> crate::compiler::CompileResult {
    serde_json::from_value(json!({
        "runId": "run", "status": status, "program": null, "runtime": null, "startedAt": 0,
        "durationMs": 1, "diagnostics": [], "hasPdf": status == "success",
        "hasSynctex": false, "message": null,
    }))
    .unwrap()
}

#[tokio::test]
async fn new_and_imported_projects_start_with_a_snapshot() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    let all = timeline(&app, &id).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["kind"], "created");
    assert_eq!(all[0]["added"], 1);
    let files = app
        .call(
            Method::GET,
            &format!("/api/projects/{id}/history/{}/diff", rev(&all[0])),
            None,
        )
        .await
        .json();
    assert_eq!(files["files"][0]["path"], "main.tex");
    assert_eq!(files["files"][0]["status"], "added");

    let archive = super::api_tests::zip_of(&[
        ("paper/main.tex", b"\\documentclass{article}"),
        ("paper/figures/plot.png", b"\x89PNG\0data"),
    ]);
    let (content_type, body) =
        super::api_tests::multipart(&[("file", Some("Paper.zip"), &archive)]);
    let reply = app
        .send(
            TestApp::request(Method::POST, "/api/projects/import")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let imported = reply.json()["project"]["id"].as_str().unwrap().to_owned();
    let all = timeline(&app, &imported).await;
    assert_eq!(all.len(), 1);
    assert_eq!(
        (all[0]["kind"].clone(), all[0]["added"].clone()),
        (json!("created"), json!(2))
    );
}

#[tokio::test]
async fn successful_compiles_are_snapshotted_at_most_every_ten_minutes() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    let compiled = |all: &[Value]| all.iter().filter(|s| s["kind"] == "compiled").count();

    for status in ["failed", "cancelled", "timedOut"] {
        put(&app, &id, "main.tex", status).await;
        crate::history::after_compile(&app.state, &id, &compile_result(status), None).await;
    }
    assert_eq!(compiled(&timeline(&app, &id).await), 0);

    crate::history::after_compile(&app.state, &id, &compile_result("success"), None).await;
    let all = timeline(&app, &id).await;
    assert_eq!(compiled(&all), 1);
    assert_eq!(all[0]["summary"], "Compiled successfully");

    put(&app, &id, "main.tex", "changed again").await;
    crate::history::after_compile(&app.state, &id, &compile_result("success"), None).await;
    assert_eq!(compiled(&timeline(&app, &id).await), 1);

    // Ten minutes later another one is taken.
    sqlx::query("UPDATE snapshots SET created_at = created_at - ? WHERE kind = 'compiled'")
        .bind(crate::history::COMPILE_SNAPSHOT_INTERVAL_MS)
        .execute(&app.state.db)
        .await
        .unwrap();
    crate::history::after_compile(&app.state, &id, &compile_result("success"), None).await;
    assert_eq!(compiled(&timeline(&app, &id).await), 2);
}

#[tokio::test]
async fn a_broken_history_does_not_break_compiles() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Paper").await;
    let git_dir = app.state.paths.history_dir(&id);
    std::fs::remove_dir_all(&git_dir).unwrap();
    std::fs::write(&git_dir, "not a repository").unwrap();

    put(&app, &id, "main.tex", "changed").await;
    // Returns normally, only logs.
    crate::history::after_compile(&app.state, &id, &compile_result("success"), None).await;
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM snapshots WHERE project_id = ? AND kind = 'compiled'",
    )
    .bind(&id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn a_project_never_snapshotted_has_an_empty_timeline() {
    let app = TestApp::new(Vec::new()).await;
    // A project from before version history, with files but no snapshot.
    let id = uuid::Uuid::now_v7().to_string();
    let now = crate::db::now_ms();
    sqlx::query(
        "INSERT INTO projects (id, name, main_file, created_at, updated_at)
         VALUES (?, 'Old paper', 'main.tex', ?, ?)",
    )
    .bind(&id)
    .bind(now)
    .bind(now)
    .execute(&app.state.db)
    .await
    .unwrap();
    let dir = app.state.paths.project_dir(&id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.tex"), "x").unwrap();

    let reply = app
        .call(Method::GET, &format!("/api/projects/{id}/history"), None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!({ "snapshots": [], "nextBefore": null }));
}
