//! Server mode: the people of a project (`project-access` spec): adding, removing, leaving,
//! handing the project over, and what the open pages hear about it.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::accounts_tests::{Reply, SITE, account, call, get, post, server, sign_in};
use crate::members::{self, ProjectRole};
use crate::state::AppState;
use crate::users::UserRole;

pub(super) fn send(method: Method, path: &str, cookie: &str, body: Option<Value>) -> Request<Body> {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::ORIGIN, SITE)
        .header(header::COOKIE, cookie);
    match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    }
}

async fn create_project(app: &Router, cookie: &str, name: &str) -> String {
    let created = call(
        app,
        post(
            "/api/projects",
            Some(cookie),
            json!({ "name": name, "template": "article" }),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    created.json["id"].as_str().unwrap().to_owned()
}

async fn project_names(app: &Router, cookie: &str) -> Vec<String> {
    call(app, get("/api/projects", Some(cookie)))
        .await
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["name"].as_str().unwrap().to_owned())
        .collect()
}

async fn role(state: &AppState, project: &str, user: &str) -> Option<ProjectRole> {
    members::role_of(&state.db, project, user).await.unwrap()
}

fn error_code(reply: &Reply) -> &str {
    reply.json["error"]["code"].as_str().unwrap_or_default()
}

/// An open event stream.
pub(super) struct Stream {
    body: Body,
    buffer: String,
}

impl Stream {
    pub(super) async fn open(app: &Router, path: &str, cookie: &str) -> Self {
        let response = app.clone().oneshot(get(path, Some(cookie))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        Self {
            body: response.into_body(),
            buffer: String::new(),
        }
    }

    /// The next event, or `None` once the stream has ended. Waits at most `wait`; `Err` when
    /// nothing came in that time.
    pub(super) async fn next(&mut self, wait: Duration) -> Result<Option<Value>, ()> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let event: String = self.buffer.drain(..end + 2).collect();
                if let Some(data) = event.lines().find_map(|line| line.strip_prefix("data: ")) {
                    return Ok(Some(serde_json::from_str(data).unwrap()));
                }
                continue;
            }
            let frame = tokio::time::timeout(wait, self.body.frame())
                .await
                .map_err(|_| ())?;
            match frame {
                None => return Ok(None),
                Some(frame) => {
                    if let Ok(data) = frame.unwrap().into_data() {
                        self.buffer.push_str(&String::from_utf8_lossy(&data));
                    }
                }
            }
        }
    }

    async fn expect(&mut self, kind: &str) {
        let event = self
            .next(Duration::from_secs(5))
            .await
            .unwrap_or_else(|()| panic!("no {kind} event in time"))
            .unwrap_or_else(|| panic!("the stream ended before {kind}"));
        assert_eq!(event["type"], kind, "{event}");
    }
}

#[tokio::test]
async fn owners_and_managers_add_and_remove_people() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::User, "lan's own password").await;
    let minh = account(&state, "minh", UserRole::User, "minh's own password").await;
    let ha = account(&state, "ha", UserRole::User, "ha's own password").await;
    let locked = account(&state, "gone", UserRole::User, "gone's own password").await;
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?")
        .bind(&locked.id)
        .execute(&state.db)
        .await
        .unwrap();
    let lan_cookie = sign_in(&app, "lan", "lan's own password").await;
    let minh_cookie = sign_in(&app, "minh", "minh's own password").await;
    let ha_cookie = sign_in(&app, "ha", "ha's own password").await;
    let id = create_project(&app, &lan_cookie, "Paper").await;
    let members = format!("/api/projects/{id}/members");
    let member = |user: &str| format!("/api/projects/{id}/members/{user}");
    let add = |cookie: &str, username: &str, role: &str| {
        send(
            Method::POST,
            &members,
            cookie,
            Some(json!({ "username": username, "role": role })),
        )
    };

    // The owner adds Minh: Paper shows up in his list, and he can work in it.
    assert!(project_names(&app, &minh_cookie).await.is_empty());
    let added = call(&app, add(&lan_cookie, "minh", "member")).await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.json);
    let listed: Vec<(&str, &str)> = added
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["username"].as_str().unwrap(), m["role"].as_str().unwrap()))
        .collect();
    assert_eq!(listed, [("lan", "owner"), ("minh", "member")]);
    assert_eq!(project_names(&app, &minh_cookie).await, ["Paper"]);
    let file = call(
        &app,
        get(
            &format!("/api/projects/{id}/files/main.tex"),
            Some(&minh_cookie),
        ),
    )
    .await;
    assert_eq!(file.status, StatusCode::OK);

    // What cannot be added.
    let again = call(&app, add(&lan_cookie, "MINH", "manager")).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(error_code(&again), "already_member");
    for (username, role) in [("nobody", "member"), ("gone", "member")] {
        let reply = call(&app, add(&lan_cookie, username, role)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{username}");
    }
    let second_owner = call(&app, add(&lan_cookie, "ha", "owner")).await;
    assert_eq!(second_owner.status, StatusCode::BAD_REQUEST);

    // A member does not manage people.
    let by_member = call(&app, add(&minh_cookie, "ha", "member")).await;
    assert_eq!(by_member.status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&by_member), "project_role");

    // Ha, a manager, manages people but cannot touch the owner.
    assert_eq!(
        call(&app, add(&lan_cookie, "ha", "manager")).await.status,
        StatusCode::CREATED
    );
    let promoted = call(
        &app,
        send(
            Method::PATCH,
            &member(&minh.id),
            &ha_cookie,
            Some(json!({ "role": "manager" })),
        ),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.json);
    assert_eq!(
        role(&state, &id, &minh.id).await,
        Some(ProjectRole::Manager)
    );
    for request in [
        send(Method::DELETE, &member(&lan.id), &ha_cookie, None),
        send(
            Method::PATCH,
            &member(&lan.id),
            &ha_cookie,
            Some(json!({ "role": "member" })),
        ),
        send(
            Method::PATCH,
            &member(&minh.id),
            &ha_cookie,
            Some(json!({ "role": "owner" })),
        ),
    ] {
        let reply = call(&app, request).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.json);
    }
    assert_eq!(role(&state, &id, &lan.id).await, Some(ProjectRole::Owner));

    // The owner cannot leave; anyone else can.
    let owner_leaves = call(
        &app,
        send(Method::DELETE, &member(&lan.id), &lan_cookie, None),
    )
    .await;
    assert_eq!(owner_leaves.status, StatusCode::BAD_REQUEST);
    assert!(
        owner_leaves.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hand the project"),
        "{}",
        owner_leaves.json
    );
    let minh_leaves = call(
        &app,
        send(Method::DELETE, &member(&minh.id), &minh_cookie, None),
    )
    .await;
    assert_eq!(minh_leaves.status, StatusCode::NO_CONTENT);
    assert!(project_names(&app, &minh_cookie).await.is_empty());

    // Ha removes himself the same way; the owner is left alone.
    assert_eq!(
        call(
            &app,
            send(Method::DELETE, &member(&ha.id), &ha_cookie, None)
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let left = call(&app, get(&members, Some(&lan_cookie))).await;
    assert_eq!(left.json.as_array().unwrap().len(), 1);
    // Someone who is not in the project is not found.
    let missing = call(
        &app,
        send(Method::DELETE, &member(&ha.id), &lan_cookie, None),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_removed_member_loses_the_project_and_its_stream() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "lan's own password").await;
    let minh = account(&state, "minh", UserRole::User, "minh's own password").await;
    account(&state, "out", UserRole::User, "outsider password").await;
    let lan_cookie = sign_in(&app, "lan", "lan's own password").await;
    let minh_cookie = sign_in(&app, "minh", "minh's own password").await;
    let out_cookie = sign_in(&app, "out", "outsider password").await;
    let id = create_project(&app, &lan_cookie, "Paper").await;

    // Minh's projects page hears that he was added.
    let mut minh_app = Stream::open(&app, "/api/events", &minh_cookie).await;
    let mut out_app = Stream::open(&app, "/api/events", &out_cookie).await;
    let added = call(
        &app,
        post(
            &format!("/api/projects/{id}/members"),
            Some(&lan_cookie),
            json!({ "username": "minh", "role": "member" }),
        ),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED);
    minh_app.expect("projectsChanged").await;

    // Minh opens Paper; so does Lan.
    let mut minh_project =
        Stream::open(&app, &format!("/api/projects/{id}/events"), &minh_cookie).await;
    let mut lan_project =
        Stream::open(&app, &format!("/api/projects/{id}/events"), &lan_cookie).await;

    let removed = call(
        &app,
        send(
            Method::DELETE,
            &format!("/api/projects/{id}/members/{}", minh.id),
            &lan_cookie,
            None,
        ),
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);

    // Minh hears it once, then his stream of Paper ends.
    minh_project.expect("membersChanged").await;
    assert_eq!(
        minh_project.next(Duration::from_secs(5)).await,
        Ok(None),
        "the stream of a removed member must end"
    );
    minh_app.expect("projectsChanged").await;
    // Lan's stream goes on: she hears the change and keeps getting events.
    lan_project.expect("membersChanged").await;
    let snapshot = call(
        &app,
        post(
            &format!("/api/projects/{id}/history"),
            Some(&lan_cookie),
            json!({ "label": "after Minh left" }),
        ),
    )
    .await;
    assert_eq!(snapshot.status, StatusCode::CREATED, "{}", snapshot.json);
    lan_project.expect("historyChanged").await;
    // People not concerned hear nothing.
    assert_eq!(out_app.next(Duration::from_millis(300)).await, Err(()));

    // Minh's next requests find nothing.
    for path in [
        format!("/api/projects/{id}"),
        format!("/api/projects/{id}/files/main.tex"),
        format!("/api/projects/{id}/events"),
    ] {
        assert_eq!(
            call(&app, get(&path, Some(&minh_cookie))).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert!(project_names(&app, &minh_cookie).await.is_empty());
}

#[tokio::test]
async fn handing_over_makes_the_old_owner_a_manager() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::User, "lan's own password").await;
    let minh = account(&state, "minh", UserRole::User, "minh's own password").await;
    let ha = account(&state, "ha", UserRole::User, "ha's own password").await;
    let lan_cookie = sign_in(&app, "lan", "lan's own password").await;
    let minh_cookie = sign_in(&app, "minh", "minh's own password").await;
    let id = create_project(&app, &lan_cookie, "Paper").await;
    members::add(&state.db, &id, &minh.id, ProjectRole::Manager, 0)
        .await
        .unwrap();
    let hand_over = |cookie: &str, user: &str| {
        send(
            Method::POST,
            &format!("/api/projects/{id}/owner"),
            cookie,
            Some(json!({ "userId": user })),
        )
    };

    // Only the owner hands the project over, and only to someone in it.
    let by_manager = call(&app, hand_over(&minh_cookie, &minh.id)).await;
    assert_eq!(by_manager.status, StatusCode::FORBIDDEN);
    let outsider = call(&app, hand_over(&lan_cookie, &ha.id)).await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND);

    let handed = call(&app, hand_over(&lan_cookie, &minh.id)).await;
    assert_eq!(handed.status, StatusCode::OK, "{}", handed.json);
    assert_eq!(handed.json[0]["username"], "minh");
    assert_eq!(handed.json[0]["role"], "owner");
    assert_eq!(role(&state, &id, &minh.id).await, Some(ProjectRole::Owner));
    assert_eq!(role(&state, &id, &lan.id).await, Some(ProjectRole::Manager));
    let owners: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_members WHERE project_id = ? AND role = 'owner'",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(owners, 1);

    // Lan is a manager now: she can no longer delete the project; Minh can.
    let delete = |cookie: &str| send(Method::DELETE, &format!("/api/projects/{id}"), cookie, None);
    assert_eq!(
        call(&app, delete(&lan_cookie)).await.status,
        StatusCode::FORBIDDEN
    );
    // Nor hand it to someone locked.
    members::add(&state.db, &id, &ha.id, ProjectRole::Member, 0)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?")
        .bind(&ha.id)
        .execute(&state.db)
        .await
        .unwrap();
    assert_eq!(
        call(&app, hand_over(&minh_cookie, &ha.id)).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, delete(&minh_cookie)).await.status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn admins_hand_over_only_the_projects_of_locked_owners() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::User, "lan's own password").await;
    let minh = account(&state, "minh", UserRole::User, "minh's own password").await;
    account(&state, "boss", UserRole::Admin, "the admin password").await;
    let lan_cookie = sign_in(&app, "lan", "lan's own password").await;
    let minh_cookie = sign_in(&app, "minh", "minh's own password").await;
    let boss = sign_in(&app, "boss", "the admin password").await;
    let paper = create_project(&app, &lan_cookie, "Paper").await;
    let other = create_project(&app, &minh_cookie, "Other").await;
    let hand_over = |cookie: &str, project: &str| {
        send(
            Method::POST,
            &format!("/api/users/{}/projects/{project}/owner", lan.id),
            cookie,
            Some(json!({ "userId": minh.id })),
        )
    };

    // The admin sees Lan's projects.
    let listed = call(
        &app,
        get(&format!("/api/users/{}/projects", lan.id), Some(&boss)),
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.json[0]["name"], "Paper");
    assert_eq!(listed.json[0]["role"], "owner");
    assert_eq!(
        call(
            &app,
            get(
                &format!("/api/users/{}/projects", lan.id),
                Some(&minh_cookie)
            )
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );

    // While Lan's account is active, she decides.
    let active = call(&app, hand_over(&boss, &paper)).await;
    assert_eq!(active.status, StatusCode::CONFLICT);
    assert_eq!(error_code(&active), "owner_active");

    let locked = call(
        &app,
        send(
            Method::PATCH,
            &format!("/api/users/{}", lan.id),
            &boss,
            Some(json!({ "disabled": true })),
        ),
    )
    .await;
    assert_eq!(locked.status, StatusCode::OK);
    assert_eq!(
        call(&app, hand_over(&minh_cookie, &paper)).await.status,
        StatusCode::FORBIDDEN
    );
    // Only projects Lan owns.
    assert_eq!(
        call(&app, hand_over(&boss, &other)).await.status,
        StatusCode::NOT_FOUND
    );

    let handed = call(&app, hand_over(&boss, &paper)).await;
    assert_eq!(handed.status, StatusCode::OK, "{}", handed.json);
    assert_eq!(
        role(&state, &paper, &minh.id).await,
        Some(ProjectRole::Owner)
    );
    assert_eq!(
        role(&state, &paper, &lan.id).await,
        Some(ProjectRole::Manager)
    );
    // Minh has Paper with its files; the admin still is not in it.
    let mut names = project_names(&app, &minh_cookie).await;
    names.sort();
    assert_eq!(names, ["Other", "Paper"]);
    let file = call(
        &app,
        get(
            &format!("/api/projects/{paper}/files/main.tex"),
            Some(&minh_cookie),
        ),
    )
    .await;
    assert_eq!(file.status, StatusCode::OK);
    assert_eq!(
        call(&app, get(&format!("/api/projects/{paper}"), Some(&boss)))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn single_user_mode_has_no_members() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open_in(
        crate::mode::Mode::Single,
        dir.path(),
        crate::auth::AccessToken::new(super::accounts_tests::TOKEN).unwrap(),
        Vec::new(),
    )
    .await
    .unwrap();
    let app = super::router(state);
    let created = call(
        &app,
        Request::post("/api/projects")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", super::accounts_tests::TOKEN),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "name": "Paper", "template": "blank" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let id = created.json["id"].as_str().unwrap();
    let members = call(
        &app,
        Request::get(format!("/api/projects/{id}/members"))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", super::accounts_tests::TOKEN),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(members.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn conversations_belong_to_whoever_started_them() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "lan's own password").await;
    let minh = account(&state, "minh", UserRole::User, "minh's own password").await;
    let lan_cookie = sign_in(&app, "lan", "lan's own password").await;
    let minh_cookie = sign_in(&app, "minh", "minh's own password").await;
    let id = create_project(&app, &lan_cookie, "Paper").await;
    members::add(&state.db, &id, &minh.id, ProjectRole::Member, 0)
        .await
        .unwrap();

    // Each starts a conversation and sends an image in it.
    let start = |cookie: &str| post("/api/ai/threads", Some(cookie), json!({ "projectId": id }));
    let lan_thread = call(&app, start(&lan_cookie)).await;
    assert_eq!(
        lan_thread.status,
        StatusCode::CREATED,
        "{}",
        lan_thread.json
    );
    let lan_thread = lan_thread.json["id"].as_str().unwrap().to_owned();
    let minh_thread = call(&app, start(&minh_cookie)).await.json["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let image = uuid::Uuid::now_v7().to_string();
    for thread in [&lan_thread, &minh_thread] {
        let dir = state.paths.thread_attachments_dir(&id, thread);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{image}.png")), b"\x89PNG\r\n\x1a\n").unwrap();
    }

    // Each sees only their own.
    let list = |cookie: &str| get(&format!("/api/ai/threads?projectId={id}"), Some(cookie));
    for (cookie, mine) in [(&lan_cookie, &lan_thread), (&minh_cookie, &minh_thread)] {
        let listed = call(&app, list(cookie)).await;
        assert_eq!(listed.status, StatusCode::OK);
        let ids: Vec<&str> = listed
            .json
            .as_array()
            .unwrap()
            .iter()
            .map(|thread| thread["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, [mine.as_str()]);
    }

    // Lan's conversation does not exist for Minh, whatever he tries; Lan's owner role does not
    // open Minh's either.
    let thread = |thread: &str, path: &str| format!("/api/ai/threads/{thread}{path}");
    for (cookie, other) in [(&minh_cookie, &lan_thread), (&lan_cookie, &minh_thread)] {
        for request in [
            get(&thread(other, ""), Some(cookie)),
            get(&thread(other, &format!("/images/{image}")), Some(cookie)),
            post(
                &thread(other, "/messages"),
                Some(cookie),
                json!({ "text": "hi" }),
            ),
            post(
                &thread(other, "/decisions"),
                Some(cookie),
                json!({ "decisions": [] }),
            ),
            send(Method::DELETE, &thread(other, ""), cookie, None),
        ] {
            let path = request.uri().to_string();
            let reply = call(&app, request).await;
            assert_eq!(
                reply.status,
                StatusCode::NOT_FOUND,
                "{path}: {}",
                reply.json
            );
        }
    }
    for (cookie, mine) in [(&lan_cookie, &lan_thread), (&minh_cookie, &minh_thread)] {
        assert_eq!(
            call(&app, get(&thread(mine, ""), Some(cookie)))
                .await
                .status,
            StatusCode::OK
        );
        let picture = call(
            &app,
            get(&thread(mine, &format!("/images/{image}")), Some(cookie)),
        )
        .await;
        assert_eq!(picture.status, StatusCode::OK);
    }

    // Removing Minh deletes his conversation and its images; Lan's stays.
    let removed = call(
        &app,
        send(
            Method::DELETE,
            &format!("/api/projects/{id}/members/{}", minh.id),
            &lan_cookie,
            None,
        ),
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let left: Vec<String> = sqlx::query_scalar("SELECT id FROM ai_threads WHERE project_id = ?")
        .bind(&id)
        .fetch_all(&state.db)
        .await
        .unwrap();
    assert_eq!(left, std::slice::from_ref(&lan_thread));
    assert!(
        !state
            .paths
            .thread_attachments_dir(&id, &minh_thread)
            .exists()
    );
    assert!(
        state
            .paths
            .thread_attachments_dir(&id, &lan_thread)
            .exists()
    );
}
