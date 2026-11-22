//! Server mode: password sign-in and sessions (`user-accounts` spec).

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::router;
use crate::auth::AccessToken;
use crate::mode::{Mode, ModeArg};
use crate::state::AppState;
use crate::users::{SessionUser, UserRole};
use crate::{passwords, sessions, users};

pub(crate) const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub(crate) const SITE: &str = "https://prism.example.org";

/// A server-mode app and its state.
pub(crate) async fn server() -> (Router, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let mode = Mode::resolve(ModeArg::Server, Some(SITE), false).unwrap();
    let state = AppState::open_in(
        mode,
        dir.path(),
        AccessToken::new(TOKEN).unwrap(),
        Vec::new(),
    )
    .await
    .unwrap();
    std::mem::forget(dir);
    (router(state.clone()), state)
}

/// An account with a password.
pub(crate) async fn account(
    state: &AppState,
    name: &str,
    role: UserRole,
    password: &str,
) -> SessionUser {
    let user = users::create(
        &state.db,
        name,
        &format!("{name} display"),
        role,
        crate::db::now_ms(),
    )
    .await
    .unwrap();
    let hash = passwords::hash(password).unwrap();
    users::set_password_hash(&state.db, &user.id, &hash)
        .await
        .unwrap();
    user
}

pub(crate) struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: serde_json::Value,
}

pub(crate) async fn call(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    Reply {
        status,
        headers,
        json,
    }
}

/// A write request from the site itself.
pub(crate) fn post(path: &str, cookie: Option<&str>, body: serde_json::Value) -> Request<Body> {
    let mut request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, SITE);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    request.body(Body::from(body.to_string())).unwrap()
}

pub(crate) fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    request.body(Body::empty()).unwrap()
}

/// Signs in and returns `openprism_session=<value>`.
pub(crate) async fn sign_in(app: &Router, name: &str, password: &str) -> String {
    let reply = call(
        app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": name, "password": password }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.json);
    let set_cookie = reply
        .headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    set_cookie.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn signs_in_with_a_password_into_a_secure_session() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::User, "correct horse battery").await;

    let reply = call(
        &app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": "LAN", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let set_cookie = reply
        .headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    for flag in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
        assert!(set_cookie.contains(flag), "{flag}: {set_cookie}");
    }
    let cookie = set_cookie.split(';').next().unwrap();

    let session = call(&app, get("/api/auth/session", Some(cookie))).await;
    assert_eq!(session.json["authenticated"], true);
    assert_eq!(session.json["mode"], "server");
    assert_eq!(session.json["user"]["id"], lan.id.as_str());
    assert_eq!(session.json["user"]["role"], "user");
    assert_eq!(
        call(&app, get("/api/projects", Some(cookie))).await.status,
        StatusCode::OK
    );

    // Only a hash of the cookie is stored.
    let value = cookie.split_once('=').unwrap().1;
    let stored: Vec<String> = sqlx::query_scalar("SELECT id_hash FROM sessions")
        .fetch_all(&state.db)
        .await
        .unwrap();
    assert_eq!(stored, vec![sessions::hash_value(value)]);
    assert!(!stored.contains(&value.to_owned()));
}

#[tokio::test]
async fn a_wrong_password_and_an_unknown_name_look_the_same() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "correct horse battery").await;
    let wrong = call(
        &app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": "lan", "password": "wrong horse battery" }),
        ),
    )
    .await;
    let unknown = call(
        &app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": "nobody", "password": "wrong horse battery" }),
        ),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status, unknown.status);
    assert_eq!(wrong.json, unknown.json);
    assert_eq!(wrong.json["error"]["code"], "invalid_credentials");
    assert!(wrong.headers.get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn a_locked_account_cannot_sign_in() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::User, "correct horse battery").await;
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?")
        .bind(&lan.id)
        .execute(&state.db)
        .await
        .unwrap();
    let reply = call(
        &app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": "lan", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.json["error"]["code"], "invalid_credentials");
}

#[tokio::test]
async fn signing_out_ends_the_session_on_the_server() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "correct horse battery").await;
    let cookie = sign_in(&app, "lan", "correct horse battery").await;
    let reply = call(
        &app,
        post("/api/auth/logout", Some(&cookie), serde_json::json!({})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    // The old cookie, kept by someone, no longer works.
    assert_eq!(
        call(&app, get("/api/projects", Some(&cookie))).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn changing_the_password_ends_the_other_sessions() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "correct horse battery").await;
    let here = sign_in(&app, "lan", "correct horse battery").await;
    let elsewhere = sign_in(&app, "lan", "correct horse battery").await;

    let short = call(
        &app,
        post("/api/auth/password", Some(&here), serde_json::json!({ "currentPassword": "correct horse battery", "newPassword": "short pass" })),
    )
    .await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST);
    assert!(
        short.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("12")
    );

    let wrong = call(
        &app,
        post("/api/auth/password", Some(&here), serde_json::json!({ "currentPassword": "not my password", "newPassword": "a brand new password" })),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);

    let changed = call(
        &app,
        post("/api/auth/password", Some(&here), serde_json::json!({ "currentPassword": "correct horse battery", "newPassword": "a brand new password" })),
    )
    .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        call(&app, get("/api/projects", Some(&here))).await.status,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, get("/api/projects", Some(&elsewhere)))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    let old = call(
        &app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": "lan", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    sign_in(&app, "lan", "a brand new password").await;
}

#[tokio::test]
async fn a_session_ends_after_thirty_days_without_use() {
    let (_app, state) = server().await;
    let lan = account(&state, "lan", UserRole::User, "correct horse battery").await;
    let day = 24 * 60 * 60 * 1000;
    let start = 1_000_000_000_000;
    let value = sessions::create(&state.db, &lan.id, Some("test"), start)
        .await
        .unwrap();

    // Used on day 29: kept alive for 30 more days from then.
    assert!(
        sessions::lookup(&state.db, &value, start + 29 * day)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        sessions::lookup(&state.db, &value, start + 58 * day)
            .await
            .unwrap()
            .is_some()
    );
    // Then 30 days without use: gone, and the row is deleted.
    assert!(
        sessions::lookup(&state.db, &value, start + 88 * day + 1)
            .await
            .unwrap()
            .is_none()
    );
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

async fn first_admin_link(state: &AppState) -> String {
    crate::links::first_admin(&state.db, crate::db::now_ms())
        .await
        .unwrap()
        .expect("a first-admin link while there is no account")
}

#[tokio::test]
async fn the_first_admin_link_works_once() {
    let (app, state) = server().await;
    let token = first_admin_link(&state).await;

    let info = call(&app, get(&format!("/api/auth/links/{token}"), None)).await;
    assert_eq!(info.status, StatusCode::OK);
    assert_eq!(info.json["kind"], "firstAdmin");

    let created = call(
        &app,
        post(
            &format!("/api/auth/links/{token}"),
            None,
            serde_json::json!({ "username": "lan", "displayName": "Lan Tran", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::NO_CONTENT, "{}", created.json);
    let cookie = created
        .headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    let cookie = cookie.split(';').next().unwrap();
    let session = call(&app, get("/api/auth/session", Some(cookie))).await;
    assert_eq!(session.json["user"]["username"], "lan");
    assert_eq!(session.json["user"]["displayName"], "Lan Tran");
    assert_eq!(session.json["user"]["role"], "admin");

    // Used: the page says so, and a second admin cannot be made with it.
    let again = call(&app, get(&format!("/api/auth/links/{token}"), None)).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(again.json["error"]["code"], "invalid_link");
    let second = call(
        &app,
        post(
            &format!("/api/auth/links/{token}"),
            None,
            serde_json::json!({ "username": "eve", "displayName": "Eve", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(second.status, StatusCode::NOT_FOUND);
    // And no new first-admin link once an account exists.
    assert!(
        crate::links::first_admin(&state.db, crate::db::now_ms())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_restart_replaces_the_first_admin_link_and_links_expire() {
    let (app, state) = server().await;
    let old = first_admin_link(&state).await;
    let new = first_admin_link(&state).await;
    assert_eq!(
        call(&app, get(&format!("/api/auth/links/{old}"), None))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, get(&format!("/api/auth/links/{new}"), None))
            .await
            .status,
        StatusCode::OK
    );

    let mut conn = state.db.acquire().await.unwrap();
    let later = crate::db::now_ms() + crate::links::FIRST_ADMIN_LIFETIME_MS + 1;
    assert!(
        crate::links::find(&mut conn, &new, later)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_set_password_link_signs_the_account_in_and_ends_old_sessions() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::Admin, "correct horse battery").await;
    let minh = account(&state, "minh", UserRole::User, "old password here").await;
    let old_session = sign_in(&app, "minh", "old password here").await;

    let first = crate::links::set_password(&state.db, &minh.id, crate::db::now_ms())
        .await
        .unwrap();
    let token = crate::links::set_password(&state.db, &minh.id, crate::db::now_ms())
        .await
        .unwrap();
    // A newer link replaces the older one.
    assert_eq!(
        call(&app, get(&format!("/api/auth/links/{first}"), None))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let info = call(&app, get(&format!("/api/auth/links/{token}"), None)).await;
    assert_eq!(info.json["kind"], "setPassword");
    assert_eq!(info.json["username"], "minh");

    let short = call(
        &app,
        post(
            &format!("/api/auth/links/{token}"),
            None,
            serde_json::json!({ "password": "too short" }),
        ),
    )
    .await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST);

    let set = call(
        &app,
        post(
            &format!("/api/auth/links/{token}"),
            None,
            serde_json::json!({ "password": "a fresh password" }),
        ),
    )
    .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT, "{}", set.json);
    assert!(set.headers.get(header::SET_COOKIE).is_some());
    assert_eq!(
        call(&app, get("/api/projects", Some(&old_session)))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    sign_in(&app, "minh", "a fresh password").await;
    assert_eq!(
        call(&app, get(&format!("/api/auth/links/{token}"), None))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_first_admin_takes_over_single_user_data() {
    let (app, state) = server().await;
    let now = crate::db::now_ms();
    for (id, name) in [("p1", "Paper"), ("p2", "Thesis"), ("p3", "Notes")] {
        sqlx::query("INSERT INTO projects (id, name, main_file, created_at, updated_at) VALUES (?, ?, 'main.tex', ?, ?)")
            .bind(id).bind(name).bind(now).bind(now)
            .execute(&state.db).await.unwrap();
    }
    sqlx::query("INSERT INTO ai_threads (id, project_id, title, created_at, updated_at) VALUES ('t1', 'p1', 'Chat', ?, ?)")
        .bind(now).bind(now).execute(&state.db).await.unwrap();
    sqlx::query("INSERT INTO jobs (id, kind, title, state, params, created_at, updated_at) VALUES ('j1', 'pdfImport', 'Import', 'succeeded', '{}', ?, ?)")
        .bind(now).bind(now).execute(&state.db).await.unwrap();

    let token = first_admin_link(&state).await;
    let created = call(
        &app,
        post(
            &format!("/api/auth/links/{token}"),
            None,
            serde_json::json!({ "username": "lan", "displayName": "Lan", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::NO_CONTENT);
    let admin: String = sqlx::query_scalar("SELECT id FROM users WHERE username = 'lan'")
        .fetch_one(&state.db)
        .await
        .unwrap();
    let owners: Vec<(String, String, String)> =
        sqlx::query_as("SELECT project_id, user_id, role FROM project_members ORDER BY project_id")
            .fetch_all(&state.db)
            .await
            .unwrap();
    assert_eq!(
        owners,
        ["p1", "p2", "p3"]
            .map(|p| (p.to_owned(), admin.clone(), "owner".to_owned()))
            .to_vec()
    );
    let thread_owner: Option<String> = sqlx::query_scalar("SELECT user_id FROM ai_threads")
        .fetch_one(&state.db)
        .await
        .unwrap();
    let job_owner: Option<String> = sqlx::query_scalar("SELECT user_id FROM jobs")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(thread_owner.as_deref(), Some(admin.as_str()));
    assert_eq!(job_owner.as_deref(), Some(admin.as_str()));
}

#[tokio::test]
async fn guessing_a_password_gets_locked_out() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "correct horse battery").await;
    let attempt = |password: &'static str| {
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": "lan", "password": password }),
        )
    };
    for _ in 0..5 {
        assert_eq!(
            call(&app, attempt("a wrong guess here")).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let sixth = call(&app, attempt("a wrong guess here")).await;
    assert_eq!(sixth.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(sixth.json["error"]["code"], "too_many_attempts");
    let wait: i64 = sixth.headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    // Even the right password waits.
    let right = call(&app, attempt("correct horse battery")).await;
    assert_eq!(right.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(right.headers.get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn writes_must_come_from_the_site() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "correct horse battery").await;
    let cookie = sign_in(&app, "lan", "correct horse battery").await;
    let create = |origin: Option<&str>| {
        let mut request = Request::post("/api/projects")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        request
            .body(Body::from(
                serde_json::json!({ "name": "Paper", "template": "article" }).to_string(),
            ))
            .unwrap()
    };
    let foreign = call(&app, create(Some("https://evil.example"))).await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    assert_eq!(foreign.json["error"]["code"], "cross_site");
    assert_eq!(call(&app, create(None)).await.status, StatusCode::FORBIDDEN);
    let projects = call(&app, get("/api/projects", Some(&cookie))).await;
    assert_eq!(
        projects.json.as_array().unwrap().len(),
        0,
        "nothing was created"
    );

    assert_eq!(
        call(&app, create(Some(SITE))).await.status,
        StatusCode::CREATED
    );
    // Reading needs no Origin.
    let read = Request::get("/api/projects")
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&app, read).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_token_signs_no_browser_in_but_scripts_still_use_it() {
    let (app, _state) = server().await;
    let login = call(
        &app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "token": TOKEN }),
        ),
    )
    .await;
    assert_eq!(login.status, StatusCode::UNAUTHORIZED);
    assert!(login.headers.get(header::SET_COOKIE).is_none());

    // A script on the machine: bearer token, no cookie, no Origin.
    let script = Request::post("/api/projects")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from(
            serde_json::json!({ "name": "From a script", "template": "article" }).to_string(),
        ))
        .unwrap();
    assert_eq!(call(&app, script).await.status, StatusCode::CREATED);
}

fn patch(path: &str, cookie: &str, body: serde_json::Value) -> Request<Body> {
    Request::patch(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, SITE)
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn delete(path: &str, cookie: &str) -> Request<Body> {
    Request::delete(path)
        .header(header::ORIGIN, SITE)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn admins_create_accounts_with_a_link() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::Admin, "correct horse battery").await;
    let admin = sign_in(&app, "lan", "correct horse battery").await;

    let created = call(
        &app,
        post(
            "/api/users",
            Some(&admin),
            serde_json::json!({ "username": "minh", "displayName": "Minh Le", "role": "user" }),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    assert_eq!(created.json["user"]["hasPassword"], false);
    let link = created.json["link"].as_str().unwrap();
    let token = link.strip_prefix(&format!("{SITE}/setup/")).expect(link);

    let taken = call(
        &app,
        post(
            "/api/users",
            Some(&admin),
            serde_json::json!({ "username": "MINH", "displayName": "Other", "role": "user" }),
        ),
    )
    .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);

    let set = call(
        &app,
        post(
            &format!("/api/auth/links/{token}"),
            None,
            serde_json::json!({ "password": "minh's own password" }),
        ),
    )
    .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT);
    let listed = call(&app, get("/api/users", Some(&admin))).await;
    let names: Vec<&str> = listed
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["lan", "minh"]);
    assert_eq!(listed.json[1]["hasPassword"], true);
}

#[tokio::test]
async fn users_cannot_manage_accounts() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::Admin, "correct horse battery").await;
    account(&state, "minh", UserRole::User, "minh's own password").await;
    let minh = sign_in(&app, "minh", "minh's own password").await;
    let attempts = [
        get("/api/users", Some(&minh)),
        post(
            "/api/users",
            Some(&minh),
            serde_json::json!({ "username": "eve", "displayName": "Eve", "role": "admin" }),
        ),
        patch(
            &format!("/api/users/{}", lan.id),
            &minh,
            serde_json::json!({ "disabled": true }),
        ),
        post(
            &format!("/api/users/{}/link", lan.id),
            Some(&minh),
            serde_json::json!({}),
        ),
    ];
    for request in attempts {
        let reply = call(&app, request).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert_eq!(reply.json["error"]["code"], "admins_only");
    }
    assert!(
        !users::get(&state.db, &lan.id)
            .await
            .unwrap()
            .unwrap()
            .disabled
    );
}

#[tokio::test]
async fn the_last_admin_stays() {
    let (app, state) = server().await;
    let lan = account(&state, "lan", UserRole::Admin, "correct horse battery").await;
    let admin = sign_in(&app, "lan", "correct horse battery").await;
    for change in [
        serde_json::json!({ "role": "user" }),
        serde_json::json!({ "disabled": true }),
    ] {
        let reply = call(
            &app,
            patch(&format!("/api/users/{}", lan.id), &admin, change),
        )
        .await;
        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(reply.json["error"]["code"], "last_admin");
    }
    // With a second admin, the first may step down.
    let ha = account(&state, "ha", UserRole::Admin, "ha's own password").await;
    let reply = call(
        &app,
        patch(
            &format!("/api/users/{}", lan.id),
            &admin,
            serde_json::json!({ "role": "user" }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json["role"], "user");
    let _ = ha;
}

#[tokio::test]
async fn locking_an_account_signs_it_out_at_once() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::Admin, "correct horse battery").await;
    let minh_user = account(&state, "minh", UserRole::User, "minh's own password").await;
    let admin = sign_in(&app, "lan", "correct horse battery").await;
    let minh = sign_in(&app, "minh", "minh's own password").await;

    let locked = call(
        &app,
        patch(
            &format!("/api/users/{}", minh_user.id),
            &admin,
            serde_json::json!({ "disabled": true }),
        ),
    )
    .await;
    assert_eq!(locked.status, StatusCode::OK);
    assert_eq!(locked.json["disabled"], true);
    assert_eq!(
        call(&app, get("/api/projects", Some(&minh))).await.status,
        StatusCode::UNAUTHORIZED
    );
    let again = call(
        &app,
        post(
            "/api/auth/login",
            None,
            serde_json::json!({ "username": "minh", "password": "minh's own password" }),
        ),
    )
    .await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);

    call(
        &app,
        patch(
            &format!("/api/users/{}", minh_user.id),
            &admin,
            serde_json::json!({ "disabled": false }),
        ),
    )
    .await;
    sign_in(&app, "minh", "minh's own password").await;
}

#[tokio::test]
async fn people_see_and_end_their_own_sessions() {
    let (app, state) = server().await;
    account(&state, "lan", UserRole::User, "correct horse battery").await;
    account(&state, "minh", UserRole::User, "minh's own password").await;
    let laptop = sign_in(&app, "lan", "correct horse battery").await;
    let phone = sign_in(&app, "lan", "correct horse battery").await;
    let minh = sign_in(&app, "minh", "minh's own password").await;

    let listed = call(&app, get("/api/auth/sessions", Some(&laptop))).await;
    let sessions = listed.json.as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    let phone_hash = sessions::hash_value(phone.split_once('=').unwrap().1);
    assert!(sessions.iter().any(|s| s["id"] == phone_hash.as_str()));

    // Someone else's session is not theirs to end.
    assert_eq!(
        call(
            &app,
            delete(&format!("/api/auth/sessions/{phone_hash}"), &minh)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            delete(&format!("/api/auth/sessions/{phone_hash}"), &laptop)
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, get("/api/projects", Some(&phone))).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, get("/api/projects", Some(&laptop))).await.status,
        StatusCode::OK
    );
}
