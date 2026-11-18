use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use openprism_core::files::{self, FileEntry, FileError, Precondition};
use openprism_core::paths::RelPath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ApiError;
use crate::projects;
use crate::state::AppState;

/// Largest single file accepted by `PUT /files/{path}`.
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
/// Largest request accepted by `POST /upload`.
const MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct TreeResponse {
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct WriteFileResponse {
    /// New content hash, also sent as the `ETag` header.
    pub version: String,
}

/// Structural changes to the project tree.
#[derive(Debug, Deserialize, TS)]
#[serde(tag = "op", rename_all = "camelCase")]
#[ts(export)]
pub enum FileOperation {
    /// An empty file.
    CreateFile {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    /// A file, or a directory with everything in it.
    Delete {
        path: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    /// Target directory, relative to the project root; empty for the root.
    #[serde(default)]
    dir: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct UploadResponse {
    pub paths: Vec<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/tree", get(tree))
        .route(
            "/projects/{id}/files/{*path}",
            get(read)
                .put(write)
                .layer(DefaultBodyLimit::max(MAX_FILE_BYTES)),
        )
        // Not under /files/: a project file may be called `ops`.
        .route("/projects/{id}/file-ops", post(operation))
        .route(
            "/projects/{id}/upload",
            post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
}

async fn tree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TreeResponse>, ApiError> {
    projects::get(&state, &id).await?;
    let root = state.paths.project_dir(&id);
    let entries = tokio::task::spawn_blocking(move || files::list_tree(&root)).await??;
    Ok(Json(TreeResponse { entries }))
}

async fn read(
    State(state): State<AppState>,
    Path((id, path)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    projects::get(&state, &id).await?;
    let rel = RelPath::parse(&path)?;
    let root = state.paths.project_dir(&id);
    let (bytes, version) = {
        let rel = rel.clone();
        tokio::task::spawn_blocking(move || files::read(&root, &rel)).await??
    };
    let mime = mime_for(&rel);
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, mime);
    insert_etag(headers, &version);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    // Project files are untrusted: never let one run as a page of this origin.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// Text formats the editor opens are served as UTF-8 text; everything else by extension.
fn mime_for(rel: &RelPath) -> HeaderValue {
    const TEXT: [&str; 22] = [
        "tex", "ltx", "latex", "sty", "cls", "bib", "bst", "bbx", "cbx", "dtx", "ins", "txt", "md",
        "tikz", "pgf", "def", "cfg", "clo", "fd", "lco", "csv", "dat",
    ];
    let extension = rel.extension().map(str::to_ascii_lowercase);
    let is_text = extension.as_deref().is_none_or(|ext| TEXT.contains(&ext));
    if is_text {
        return HeaderValue::from_static("text/plain; charset=utf-8");
    }
    let guess = mime_guess::from_path(rel.as_str()).first_or_octet_stream();
    HeaderValue::from_str(guess.essence_str())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

fn insert_etag(headers: &mut HeaderMap, version: &str) {
    if let Ok(value) = HeaderValue::from_str(&format!("\"{version}\"")) {
        headers.insert(header::ETAG, value);
    }
}

/// `If-Match: "<version>"` → must match; `If-None-Match: *` → must not exist; else anything.
fn precondition(headers: &HeaderMap) -> Result<Precondition, ApiError> {
    if let Some(value) = headers.get(header::IF_MATCH) {
        let value = value
            .to_str()
            .map_err(|_| ApiError::bad_request("invalid If-Match header"))?
            .trim();
        let value = value.strip_prefix("W/").unwrap_or(value);
        return Ok(Precondition::Version(value.trim_matches('"').to_owned()));
    }
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| value.as_bytes() == b"*")
    {
        return Ok(Precondition::Absent);
    }
    Ok(Precondition::Any)
}

async fn write(
    State(state): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    projects::get(&state, &id).await?;
    let rel = RelPath::parse(&path)?;
    let precondition = precondition(&headers)?;
    let root = state.paths.project_dir(&id);
    let result = {
        let _guard = state.lock_project(&id).await;
        let rel = rel.clone();
        tokio::task::spawn_blocking(move || files::write(&root, &rel, &body, &precondition)).await?
    };
    match result {
        Ok(version) => {
            let mut response = Json(WriteFileResponse {
                version: version.clone(),
            })
            .into_response();
            insert_etag(response.headers_mut(), &version);
            Ok(response)
        }
        Err(FileError::VersionConflict { current }) => {
            let mut response = ApiError::from(FileError::VersionConflict {
                current: current.clone(),
            })
            .into_response();
            if let Some(current) = current {
                insert_etag(response.headers_mut(), &current);
            }
            Ok(response)
        }
        Err(err) => Err(err.into()),
    }
}

async fn operation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(operation): Json<FileOperation>,
) -> Result<StatusCode, ApiError> {
    projects::get(&state, &id).await?;
    let root = state.paths.project_dir(&id);
    let _guard = state.lock_project(&id).await;
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        match operation {
            FileOperation::CreateFile { path } => {
                files::write(&root, &RelPath::parse(&path)?, b"", &Precondition::Absent)?;
            }
            FileOperation::Mkdir { path } => files::create_dir(&root, &RelPath::parse(&path)?)?,
            FileOperation::Rename { from, to } => {
                files::rename(&root, &RelPath::parse(&from)?, &RelPath::parse(&to)?)?;
            }
            FileOperation::Delete { path } => files::delete(&root, &RelPath::parse(&path)?)?,
        }
        Ok(())
    })
    .await??;
    Ok(StatusCode::NO_CONTENT)
}

/// `multipart/form-data` with one or more `file` fields; existing files are replaced.
async fn upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, ApiError> {
    projects::get(&state, &id).await?;
    let dir = match query.dir.trim_matches('/') {
        "" => None,
        dir => Some(RelPath::parse(dir)?),
    };
    let root = state.paths.project_dir(&id);
    let mut written = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(err.body_text()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let file_name = field
            .file_name()
            .map(str::to_owned)
            .ok_or_else(|| ApiError::bad_request("an uploaded file has no name"))?;
        // Browsers may send "folder/name" for directory uploads; keep only the name.
        let file_name = file_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_owned();
        let rel = match &dir {
            Some(dir) => dir.join(&file_name)?,
            None => RelPath::parse(&file_name)?,
        };
        let bytes = field
            .bytes()
            .await
            .map_err(|err| ApiError::bad_request(err.body_text()))?;
        let _guard = state.lock_project(&id).await;
        let target_root = root.clone();
        let target = rel.clone();
        tokio::task::spawn_blocking(move || {
            files::write(&target_root, &target, &bytes, &Precondition::Any)
        })
        .await??;
        written.push(rel.as_str().to_owned());
    }
    if written.is_empty() {
        return Err(ApiError::bad_request("no `file` field in the upload"));
    }
    Ok(Json(UploadResponse { paths: written }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_files_are_utf8_text() {
        for (path, expected) in [
            ("main.tex", "text/plain; charset=utf-8"),
            ("refs.bib", "text/plain; charset=utf-8"),
            ("latexmkrc", "text/plain; charset=utf-8"),
            ("fig.png", "image/png"),
            ("paper.pdf", "application/pdf"),
            ("page.html", "text/html"),
        ] {
            assert_eq!(mime_for(&RelPath::parse(path).unwrap()), expected, "{path}");
        }
    }

    #[test]
    fn parses_preconditions() {
        let mut headers = HeaderMap::new();
        assert_eq!(precondition(&headers).unwrap(), Precondition::Any);
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert_eq!(precondition(&headers).unwrap(), Precondition::Absent);
        headers.insert(header::IF_MATCH, HeaderValue::from_static("W/\"abc\""));
        assert_eq!(
            precondition(&headers).unwrap(),
            Precondition::Version("abc".into())
        );
    }
}
