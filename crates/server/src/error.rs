use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use ts_rs::TS;

/// JSON body of every error response: `{ "error": { "code": ..., "message": ... } }`.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ErrorDetail {
    /// Stable, machine-readable code such as `not_found` or `forbidden`.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code.to_owned(),
                message: self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// Logs the cause and hides it from the client.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(%cause, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error, see the API log",
        )
    }
}

impl From<openprism_core::paths::PathError> for ApiError {
    fn from(err: openprism_core::paths::PathError) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_path", err.to_string())
    }
}

impl From<openprism_core::files::FileError> for ApiError {
    fn from(err: openprism_core::files::FileError) -> Self {
        use openprism_core::files::FileError as E;
        match err {
            E::Path(err) => err.into(),
            E::NotFound(_) => Self::not_found(err.to_string()),
            E::AlreadyExists(_) => Self::conflict("already_exists", err.to_string()),
            E::IsDirectory(_) | E::NotDirectory(_) | E::IntoItself(_) => {
                Self::bad_request(err.to_string())
            }
            E::VersionConflict { .. } => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "version_conflict",
                err.to_string(),
            ),
            E::TooManyEntries => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "too_many_files",
                err.to_string(),
            ),
            E::Io(err) => Self::internal(err),
        }
    }
}

impl From<openprism_core::archive::ArchiveError> for ApiError {
    fn from(err: openprism_core::archive::ArchiveError) -> Self {
        use openprism_core::archive::ArchiveError as E;
        match err {
            E::Io(err) => Self::internal(err),
            other => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_archive",
                other.to_string(),
            ),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(err)
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(err: tokio::task::JoinError) -> Self {
        Self::internal(err)
    }
}
