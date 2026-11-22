//! Access-token authentication, in the spirit of Jupyter.
//!
//! On first start a random 256-bit token is written to `<data dir>/access-token` (mode 0600).
//! A browser exchanges it once for a session cookie; scripts can send it as a bearer token.
//! The cookie holds `HMAC-SHA256(token, "openprism-session-v1")`, so sessions survive restarts
//! and are all revoked by replacing the token file.
//!
//! IP-based "local only" checks are not used: the Next.js server fills `X-Forwarded-For` only
//! when the client did not send one, so the client address cannot be trusted (checked
//! against Next.js 16.4: a client-supplied `X-Forwarded-For` reaches the API unchanged).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::mode::Mode;
use crate::state::AppState;
use crate::users::{SessionUser, UserRole};

pub const SESSION_COOKIE: &str = "openprism_session";
pub const TOKEN_FILE: &str = "access-token";
pub const TOKEN_ENV: &str = "OPENPRISM_ACCESS_TOKEN";
const MIN_TOKEN_LEN: usize = 32;
const SESSION_CONTEXT: &[u8] = b"openprism-session-v1";

/// The secret that unlocks the API. Never logged.
#[derive(Clone)]
pub struct AccessToken(Arc<str>);

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

impl AccessToken {
    pub fn new(secret: impl Into<Arc<str>>) -> anyhow::Result<Self> {
        let secret = secret.into();
        anyhow::ensure!(
            secret.len() >= MIN_TOKEN_LEN,
            "access token must be at least {MIN_TOKEN_LEN} characters"
        );
        Ok(Self(secret))
    }

    /// Uses `OPENPRISM_ACCESS_TOKEN` when set, otherwise reads (or creates) the token file.
    pub async fn resolve(data_dir: &Path) -> anyhow::Result<Self> {
        match std::env::var(TOKEN_ENV) {
            Ok(secret) if !secret.is_empty() => Self::new(secret),
            _ => Self::load_or_create(&token_path(data_dir)).await,
        }
    }

    pub async fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => Self::new(contents.trim().to_owned())
                .with_context(|| format!("invalid token in {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let token = Self::generate()?;
                write_private(path, &token.0).await?;
                tracing::info!(path = %path.display(), "created a new access token");
                Ok(token)
            }
            Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    fn generate() -> anyhow::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|err| anyhow::anyhow!("no secure randomness available: {err}"))?;
        Self::new(hex::encode(bytes))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison with a candidate token.
    pub fn matches(&self, candidate: &str) -> bool {
        self.0.as_bytes().ct_eq(candidate.as_bytes()).into()
    }

    fn mac(&self) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(self.0.as_bytes())
            .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
        mac.update(SESSION_CONTEXT);
        mac
    }

    /// Value stored in the session cookie.
    pub fn session_value(&self) -> String {
        hex::encode(self.mac().finalize().into_bytes())
    }

    pub fn verify_session(&self, value: &str) -> bool {
        hex::decode(value).is_ok_and(|tag| self.mac().verify_slice(&tag).is_ok())
    }
}

pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE)
}

async fn write_private(path: &Path, contents: &str) -> anyhow::Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .await
        .with_context(|| format!("cannot create {}", path.display()))?;
    tokio::io::AsyncWriteExt::write_all(&mut file, contents.as_bytes()).await?;
    // A tokio file hands writes to a background thread: wait for this one to land (and reach
    // the disk), or a read right after, like `print-token`, can find the file still empty.
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    file.sync_all().await?;
    Ok(())
}

/// Endpoints reachable without a session.
fn is_public(path: &str) -> bool {
    path == "/api/health" || path.starts_with("/api/auth/")
}

/// Who sends a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// Single-user mode, or the access token sent as a bearer token (scripts on the machine
    /// itself): may do everything.
    Local,
    /// A signed-in account (server mode).
    User(SessionUser),
}

impl Principal {
    /// The account, or `None` for [`Principal::Local`], whose rows keep NULL owners.
    pub fn user(&self) -> Option<&SessionUser> {
        match self {
            Self::Local => None,
            Self::User(user) => Some(user),
        }
    }

    pub fn user_id(&self) -> Option<&str> {
        self.user().map(|user| user.id.as_str())
    }

    /// May manage the server: admins, and [`Principal::Local`].
    pub fn is_admin(&self) -> bool {
        self.user().is_none_or(|user| user.role == UserRole::Admin)
    }
}

/// The principal behind a request, or `None` when it is not signed in.
pub async fn identify(state: &AppState, jar: &CookieJar, headers: &HeaderMap) -> Option<Principal> {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| state.token.matches(candidate.trim()));
    if bearer {
        return Some(Principal::Local);
    }
    let cookie = jar.get(SESSION_COOKIE)?;
    match &state.mode {
        Mode::Single => state
            .token
            .verify_session(cookie.value())
            .then_some(Principal::Local),
        Mode::Server(_) => crate::sessions::lookup(&state.db, cookie.value(), crate::db::now_ms())
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "cannot read the session");
                None
            })
            .map(Principal::User),
    }
}

/// Middleware: every non-public request needs a valid session cookie or bearer token. The
/// principal goes into the request's extensions for [`CurrentUser`].
pub async fn require_session(
    State(state): State<AppState>,
    jar: CookieJar,
    mut request: Request,
    next: Next,
) -> Response {
    if let Err(err) = check_origin(&state, &jar, &request) {
        return err.into_response();
    }
    let principal = identify(&state, &jar, request.headers()).await;
    let public = is_public(request.uri().path());
    match principal {
        Some(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        None if public => next.run(request).await,
        None => ApiError::unauthorized("sign in to Open Prism first").into_response(),
    }
}

/// Server mode: a write request must come from the site itself (design D5). Browsers send
/// `Origin` with every write; together with `SameSite=Strict` cookies this stops cross-site
/// requests. A request without a session cookie that carries the access token as a bearer
/// token is a script on the machine, not a browser, and passes.
fn check_origin(state: &AppState, jar: &CookieJar, request: &Request) -> Result<(), ApiError> {
    let Some(server) = state.mode.server() else {
        return Ok(());
    };
    let method = request.method();
    if method.is_safe() {
        return Ok(());
    }
    let has_cookie = jar.get(SESSION_COOKIE).is_some();
    if !has_cookie && request.headers().contains_key(AUTHORIZATION) {
        // A script with the token: it gets no further unless the token is right.
        return Ok(());
    }
    let origin = request
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let refused = match origin {
        Some(origin) => origin != server.public_url.origin(),
        // Without a cookie there is nothing a forged request could use (signing in, say).
        None => has_cookie,
    };
    if refused {
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "cross_site",
            "this request does not come from Open Prism's own pages",
        ));
    }
    Ok(())
}

/// The principal of a request that passed [`require_session`].
#[derive(Debug, Clone)]
pub struct CurrentUser(pub Principal);

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .map(Self)
            .ok_or_else(|| ApiError::unauthorized("sign in to Open Prism first"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> AccessToken {
        AccessToken::new("a".repeat(64)).unwrap()
    }

    #[test]
    fn rejects_short_tokens() {
        assert!(AccessToken::new("short").is_err());
    }

    #[test]
    fn matches_only_the_exact_token() {
        let token = token();
        assert!(token.matches(&"a".repeat(64)));
        assert!(!token.matches(&"a".repeat(63)));
        assert!(!token.matches(&"b".repeat(64)));
    }

    #[test]
    fn session_value_round_trips_and_resists_tampering() {
        let token = token();
        let value = token.session_value();
        assert!(token.verify_session(&value));
        assert!(!token.verify_session("not-hex"));
        assert!(
            !AccessToken::new("c".repeat(64))
                .unwrap()
                .verify_session(&value)
        );
    }

    #[test]
    fn debug_output_hides_the_secret() {
        assert!(!format!("{:?}", token()).contains("aaaa"));
    }

    #[tokio::test]
    async fn creates_a_private_token_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = token_path(dir.path());

        let first = AccessToken::load_or_create(&path).await.unwrap();
        let second = AccessToken::load_or_create(&path).await.unwrap();

        assert_eq!(first.expose(), second.expose());
        assert_eq!(first.expose().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
