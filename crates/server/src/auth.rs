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
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::state::AppState;

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

/// Middleware: every non-public request needs a valid session cookie or bearer token.
pub async fn require_session(
    State(state): State<AppState>,
    jar: CookieJar,
    request: Request,
    next: Next,
) -> Response {
    if is_public(request.uri().path()) || is_authenticated(&state.token, &jar, &request) {
        return next.run(request).await;
    }
    ApiError::unauthorized("sign in with the Open Prism access token").into_response()
}

pub fn is_authenticated(token: &AccessToken, jar: &CookieJar, request: &Request) -> bool {
    let bearer = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| token.matches(candidate.trim()));
    bearer
        || jar
            .get(SESSION_COOKIE)
            .is_some_and(|c| token.verify_session(c.value()))
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
