mod ai;
#[cfg(test)]
mod ai_tests;
#[cfg(test)]
mod api_tests;
mod auth;
mod compile;
mod files;
mod health;
mod history;
#[cfg(test)]
mod history_tests;
mod imports;
mod jobs;
#[cfg(test)]
mod jobs_tests;
#[cfg(test)]
mod pdf_import_tests;
mod projects;
mod settings;
mod synctex;

use axum::Router;
use axum::middleware;
use tower_http::trace::TraceLayer;

use crate::error::ApiError;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .merge(health::routes())
        .merge(auth::routes())
        .merge(projects::routes())
        .merge(files::routes())
        .merge(compile::routes())
        .merge(history::routes())
        .merge(synctex::routes())
        .merge(settings::routes())
        .merge(ai::routes())
        .merge(jobs::routes())
        .merge(imports::routes());

    Router::new()
        .nest("/api", api)
        .fallback(|| async { ApiError::not_found("no such endpoint") })
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_session,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::auth::{AccessToken, SESSION_COOKIE};

    pub(crate) const TOKEN: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    async fn app() -> Router {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(dir.path(), AccessToken::new(TOKEN).unwrap(), Vec::new())
            .await
            .unwrap();
        // Keep the directory for the lifetime of the test process.
        std::mem::forget(dir);
        router(state)
    }

    async fn send(request: Request<Body>) -> (StatusCode, header::HeaderMap, serde_json::Value) {
        let response = app().await.oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, json)
    }

    fn get(path: &str) -> axum::http::request::Builder {
        Request::get(path)
    }

    fn login(token: &str) -> Request<Body> {
        Request::post("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "token": token }).to_string(),
            ))
            .unwrap()
    }

    fn session_cookie() -> String {
        let value = AccessToken::new(TOKEN).unwrap().session_value();
        format!("{SESSION_COOKIE}={value}")
    }

    #[tokio::test]
    async fn health_is_public() {
        let (status, _, body) = send(get("/api/health").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["engines"].is_array());
    }

    #[tokio::test]
    async fn everything_else_requires_a_session() {
        let (status, _, body) = send(get("/api/nope").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn session_cookie_and_bearer_token_are_accepted() {
        let with_cookie = get("/api/nope").header(header::COOKIE, session_cookie());
        let (status, _, body) = send(with_cookie.body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");

        let with_bearer = get("/api/nope").header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        let (status, _, _) = send(with_bearer.body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn forged_cookie_is_rejected() {
        let forged = get("/api/nope").header(header::COOKIE, format!("{SESSION_COOKIE}=deadbeef"));
        let (status, _, _) = send(forged.body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_sets_a_strict_http_only_cookie() {
        let (status, headers, _) = send(login(TOKEN)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(cookie.starts_with(&session_cookie()), "{cookie}");
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
    }

    #[tokio::test]
    async fn login_with_wrong_token_fails() {
        let (status, headers, body) = send(login("wrong-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "invalid_token");
        assert!(headers.get(header::SET_COOKIE).is_none());
    }

    #[tokio::test]
    async fn session_endpoint_reports_state() {
        let (_, _, anonymous) = send(get("/api/auth/session").body(Body::empty()).unwrap()).await;
        assert_eq!(anonymous["authenticated"], false);

        let signed_in = get("/api/auth/session").header(header::COOKIE, session_cookie());
        let (_, _, body) = send(signed_in.body(Body::empty()).unwrap()).await;
        assert_eq!(body["authenticated"], true);
    }
}
