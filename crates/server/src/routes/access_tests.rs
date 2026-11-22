//! Server mode: who may use which project API (`project-access` spec, design D6).
//!
//! `routes()` lists every route that reaches a project, with a request for it. A second test
//! reads the route tables of the source and fails when a project route is missing here, so a
//! new route cannot skip the access check unnoticed.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};

use super::accounts_tests::{SITE, account, call, get, post, server, sign_in};
use crate::members::{self, ProjectRole};
use crate::users::UserRole;

/// (route pattern as in the source, method, concrete path, JSON body).
fn routes(
    id: &str,
    thread: &str,
) -> Vec<(&'static str, Method, String, Option<serde_json::Value>)> {
    let run = "01a1fe00-0000-7000-8000-000000000000";
    let rev = "0123456789abcdef0123456789abcdef01234567";
    let project = |path: &str| format!("/api/projects/{id}{path}");
    vec![
        ("/projects/{id}", Method::GET, project(""), None),
        (
            "/projects/{id}",
            Method::PATCH,
            project(""),
            Some(serde_json::json!({ "name": "Renamed" })),
        ),
        ("/projects/{id}", Method::DELETE, project(""), None),
        (
            "/projects/{id}/export",
            Method::GET,
            project("/export"),
            None,
        ),
        ("/projects/{id}/tree", Method::GET, project("/tree"), None),
        (
            "/projects/{id}/files/{*path}",
            Method::GET,
            project("/files/main.tex"),
            None,
        ),
        (
            "/projects/{id}/files/{*path}",
            Method::PUT,
            project("/files/main.tex"),
            None,
        ),
        (
            "/projects/{id}/file-ops",
            Method::POST,
            project("/file-ops"),
            Some(serde_json::json!({ "op": "createFile", "path": "x.tex" })),
        ),
        (
            "/projects/{id}/upload",
            Method::POST,
            project("/upload?dir="),
            None,
        ),
        (
            "/projects/{id}/compile",
            Method::POST,
            project("/compile"),
            Some(serde_json::json!({})),
        ),
        (
            "/projects/{id}/compile/latest",
            Method::GET,
            project("/compile/latest"),
            None,
        ),
        (
            "/projects/{id}/compile/{run_id}",
            Method::GET,
            project(&format!("/compile/{run}")),
            None,
        ),
        (
            "/projects/{id}/runs/{run_id}/pages",
            Method::GET,
            project(&format!("/runs/{run}/pages")),
            None,
        ),
        (
            "/projects/{id}/runs/{run_id}/{file}",
            Method::GET,
            project(&format!("/runs/{run}/output.pdf")),
            None,
        ),
        (
            "/projects/{id}/events",
            Method::GET,
            project("/events"),
            None,
        ),
        (
            "/projects/{id}/history",
            Method::GET,
            project("/history"),
            None,
        ),
        (
            "/projects/{id}/history",
            Method::POST,
            project("/history"),
            Some(serde_json::json!({ "label": "x" })),
        ),
        (
            "/projects/{id}/history/{rev}",
            Method::PATCH,
            project(&format!("/history/{rev}")),
            Some(serde_json::json!({ "label": "y" })),
        ),
        (
            "/projects/{id}/history/{rev}/diff",
            Method::GET,
            project(&format!("/history/{rev}/diff")),
            None,
        ),
        (
            "/projects/{id}/history/{rev}/restore",
            Method::POST,
            project(&format!("/history/{rev}/restore")),
            Some(serde_json::json!({})),
        ),
        (
            "/projects/{id}/runs/{run_id}/synctex/forward",
            Method::GET,
            project(&format!("/runs/{run}/synctex/forward?file=main.tex&line=1")),
            None,
        ),
        (
            "/projects/{id}/runs/{run_id}/synctex/inverse",
            Method::GET,
            project(&format!("/runs/{run}/synctex/inverse?page=1&x=1&y=1")),
            None,
        ),
        (
            "/ai/threads",
            Method::GET,
            format!("/api/ai/threads?projectId={id}"),
            None,
        ),
        (
            "/ai/threads",
            Method::POST,
            "/api/ai/threads".into(),
            Some(serde_json::json!({ "projectId": id })),
        ),
        (
            "/ai/threads/{id}",
            Method::GET,
            format!("/api/ai/threads/{thread}"),
            None,
        ),
        (
            "/ai/threads/{id}",
            Method::DELETE,
            format!("/api/ai/threads/{thread}"),
            None,
        ),
        (
            "/ai/threads/{id}/messages",
            Method::POST,
            format!("/api/ai/threads/{thread}/messages"),
            Some(serde_json::json!({ "text": "hi" })),
        ),
        (
            "/ai/threads/{id}/decisions",
            Method::POST,
            format!("/api/ai/threads/{thread}/decisions"),
            Some(serde_json::json!({ "decisions": [] })),
        ),
        (
            "/ai/threads/{id}/images/{image_id}",
            Method::GET,
            format!("/api/ai/threads/{thread}/images/x"),
            None,
        ),
        (
            "/ai/inline",
            Method::POST,
            "/api/ai/inline".into(),
            Some(serde_json::json!({ "projectId": id, "action": "rewrite", "text": "hi" })),
        ),
        (
            "/ai/complete",
            Method::POST,
            "/api/ai/complete".into(),
            Some(
                serde_json::json!({ "projectId": id, "path": "main.tex", "prefix": "", "suffix": "" }),
            ),
        ),
        (
            "/ai/image-to-latex",
            Method::POST,
            "/api/ai/image-to-latex".into(),
            Some(
                serde_json::json!({ "projectId": id, "path": "main.tex", "image": { "data": "" }, "before": "", "after": "" }),
            ),
        ),
    ]
}

fn request(
    method: &Method,
    path: &str,
    body: Option<&serde_json::Value>,
    cookie: &str,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method.clone())
        .uri(path)
        .header(header::ORIGIN, SITE)
        .header(header::COOKIE, cookie);
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None if path.contains("/upload") => builder
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=x")
            .body(Body::from("--x--\r\n"))
            .unwrap(),
        None => builder
            .body(Body::from("\\documentclass{article}"))
            .unwrap(),
    }
}

#[tokio::test]
async fn people_outside_a_project_find_nothing() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "lan's own password").await;
    account(&state, "out", UserRole::User, "outsider password").await;
    account(&state, "boss", UserRole::Admin, "the admin password").await;
    let lan = sign_in(&app, "lan", "lan's own password").await;

    let created = call(
        &app,
        post(
            "/api/projects",
            Some(&lan),
            serde_json::json!({ "name": "Paper", "template": "article" }),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let id = created.json["id"].as_str().unwrap().to_owned();
    let thread = call(
        &app,
        post(
            "/api/ai/threads",
            Some(&lan),
            serde_json::json!({ "projectId": id }),
        ),
    )
    .await;
    assert_eq!(thread.status, StatusCode::CREATED, "{}", thread.json);
    let thread = thread.json["id"].as_str().unwrap().to_owned();

    for who in [("out", "outsider password"), ("boss", "the admin password")] {
        let cookie = sign_in(&app, who.0, who.1).await;
        let list = call(&app, get("/api/projects", Some(&cookie))).await;
        assert_eq!(list.json.as_array().unwrap().len(), 0, "{}", who.0);
        for (pattern, method, path, body) in routes(&id, &thread) {
            let reply = call(&app, request(&method, &path, body.as_ref(), &cookie)).await;
            assert_eq!(
                reply.status,
                StatusCode::NOT_FOUND,
                "{} {method} {pattern}: {}",
                who.0,
                reply.json
            );
        }
    }
    // Lan still has everything.
    let shown = call(&app, get(&format!("/api/projects/{id}"), Some(&lan))).await;
    assert_eq!(shown.status, StatusCode::OK);
    assert_eq!(shown.json["name"], "Paper");
}

#[tokio::test]
async fn roles_decide_who_configures_and_who_deletes() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "lan's own password").await;
    let minh = account(&state, "minh", UserRole::User, "minh's own password").await;
    let ha = account(&state, "ha", UserRole::User, "ha's own password").await;
    let lan_cookie = sign_in(&app, "lan", "lan's own password").await;
    let created = call(
        &app,
        post(
            "/api/projects",
            Some(&lan_cookie),
            serde_json::json!({ "name": "Paper", "template": "article" }),
        ),
    )
    .await;
    let id = created.json["id"].as_str().unwrap().to_owned();
    let now = crate::db::now_ms();
    members::add(&state.db, &id, &minh.id, ProjectRole::Member, now)
        .await
        .unwrap();
    members::add(&state.db, &id, &ha.id, ProjectRole::Manager, now)
        .await
        .unwrap();
    let minh_cookie = sign_in(&app, "minh", "minh's own password").await;
    let ha_cookie = sign_in(&app, "ha", "ha's own password").await;

    let rename = |cookie: &str, name: &str| {
        request(
            &Method::PATCH,
            &format!("/api/projects/{id}"),
            Some(&serde_json::json!({ "name": name })),
            cookie,
        )
    };
    let remove = |cookie: &str| {
        request(
            &Method::DELETE,
            &format!("/api/projects/{id}"),
            None,
            cookie,
        )
    };

    // A member uses the project but does not configure or delete it.
    assert_eq!(
        call(
            &app,
            get(
                &format!("/api/projects/{id}/files/main.tex"),
                Some(&minh_cookie)
            )
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, get("/api/projects", Some(&minh_cookie)))
            .await
            .json
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let denied = call(&app, rename(&minh_cookie, "By Minh")).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.json["error"]["code"], "project_role");
    assert_eq!(
        call(&app, remove(&minh_cookie)).await.status,
        StatusCode::FORBIDDEN
    );

    // A manager renames it, but only the owner deletes it.
    let renamed = call(&app, rename(&ha_cookie, "By Ha")).await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert_eq!(renamed.json["name"], "By Ha");
    assert_eq!(
        call(&app, remove(&ha_cookie)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, remove(&lan_cookie)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &app,
            get(&format!("/api/projects/{id}"), Some(&minh_cookie))
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn whoever_creates_or_imports_a_project_owns_it() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::User, "lan's own password").await;
    let cookie = sign_in(&app, "lan", "lan's own password").await;
    let created = call(
        &app,
        post(
            "/api/projects",
            Some(&cookie),
            serde_json::json!({ "name": "Paper", "template": "blank" }),
        ),
    )
    .await;
    let id = created.json["id"].as_str().unwrap();
    assert_eq!(
        members::role_of(&state.db, id, &lan.id).await.unwrap(),
        Some(ProjectRole::Owner)
    );

    // A ZIP import.
    let zip = {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut out);
        writer
            .start_file("main.tex", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"\\documentclass{article}").unwrap();
        writer.finish().unwrap();
        out.into_inner()
    };
    let mut body = b"--x\r\nContent-Disposition: form-data; name=\"file\"; filename=\"paper.zip\"\r\nContent-Type: application/zip\r\n\r\n".to_vec();
    body.extend_from_slice(&zip);
    body.extend_from_slice(b"\r\n--x--\r\n");
    let import = Request::post("/api/projects/import")
        .header(header::ORIGIN, SITE)
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=x")
        .body(Body::from(body))
        .unwrap();
    let imported = call(&app, import).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.json);
    let id = imported.json["project"]["id"].as_str().unwrap();
    assert_eq!(
        members::role_of(&state.db, id, &lan.id).await.unwrap(),
        Some(ProjectRole::Owner)
    );
}

/// Every route of `source` whose path holds a project (`{id}` under `/projects`, or under
/// `/ai/`).
fn project_routes(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(".route(") {
        rest = &rest[start + 7..];
        let Some(open) = rest.find('"') else { break };
        let Some(close) = rest[open + 1..].find('"') else {
            break;
        };
        let path = &rest[open + 1..open + 1 + close];
        if path.starts_with("/projects/{id}") || path.starts_with("/ai/") {
            found.push(path.to_owned());
        }
    }
    found
}

#[test]
fn every_project_route_is_in_the_table() {
    let listed: std::collections::BTreeSet<&str> = routes("p", "t")
        .into_iter()
        .map(|(pattern, ..)| pattern)
        .collect();
    let sources = [
        include_str!("projects.rs"),
        include_str!("files.rs"),
        include_str!("compile.rs"),
        include_str!("history.rs"),
        include_str!("synctex.rs"),
        include_str!("ai.rs"),
    ];
    let mut missing = Vec::new();
    for source in sources {
        for path in project_routes(source) {
            if !listed.contains(path.as_str()) {
                missing.push(path);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "routes reaching a project but not checked in `routes()`: {missing:?}"
    );
}
