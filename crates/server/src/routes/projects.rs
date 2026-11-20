use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use openprism_core::archive::{ImportLimits, export_7z, export_zip, import_zip};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use ts_rs::TS;

use crate::error::ApiError;
use crate::projects::{self, CreateProjectRequest, Project, UpdateProjectRequest};
use crate::state::AppState;

/// Largest ZIP accepted by `POST /projects/import`.
pub const MAX_IMPORT_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportResponse {
    pub project: Project,
    /// Archive entries that were not extracted, e.g. `__MACOSX/…` or unsafe paths.
    pub skipped: Vec<SkippedEntry>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct SkippedEntry {
    pub name: String,
    pub reason: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list).post(create))
        .route(
            "/projects/import",
            post(import).layer(DefaultBodyLimit::max(MAX_IMPORT_BYTES)),
        )
        .route("/projects/{id}", get(show).patch(update).delete(remove))
        .route("/projects/{id}/export", get(export))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(projects::list(&state).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(projects::create(&state, request).await?),
    ))
}

async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(projects::get(&state, &id).await?))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateProjectRequest>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(projects::update(&state, &id, request).await?))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    projects::delete(&state, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `multipart/form-data` with a `file` (the ZIP) and an optional `name`.
async fn import(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ImportResponse>), ApiError> {
    let mut name: Option<String> = None;
    let mut upload: Option<(tempfile::NamedTempFile, Option<String>)> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(err.body_text()))?
    {
        match field.name() {
            Some("name") => {
                name = Some(
                    field
                        .text()
                        .await
                        .map_err(|err| ApiError::bad_request(err.body_text()))?,
                );
            }
            Some("file") => {
                let file_name = field.file_name().map(str::to_owned);
                let temp = tempfile::NamedTempFile::new_in(&state.paths.tmp)
                    .map_err(ApiError::internal)?;
                let mut out = tokio::fs::File::from_std(temp.reopen().map_err(ApiError::internal)?);
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|err| ApiError::bad_request(err.body_text()))?
                {
                    out.write_all(&chunk).await.map_err(ApiError::internal)?;
                }
                out.flush().await.map_err(ApiError::internal)?;
                upload = Some((temp, file_name));
            }
            _ => {}
        }
    }
    let Some((temp, file_name)) = upload else {
        return Err(ApiError::bad_request(
            "missing the `file` field with the ZIP archive",
        ));
    };
    let name = name
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            file_name.as_deref().map(|file| {
                file.strip_suffix(".zip")
                    .or_else(|| file.strip_suffix(".ZIP"))
                    .unwrap_or(file)
                    .to_owned()
            })
        })
        .unwrap_or_else(|| "Imported project".to_owned());

    let (report_tx, report_rx) = tokio::sync::oneshot::channel();
    let project = projects::create_from_dir(&state, &name, move |dir| {
        let file = temp.reopen().map_err(ApiError::internal)?;
        let report = import_zip(std::io::BufReader::new(file), dir, ImportLimits::default())?;
        let main_file = report.main_file.clone();
        let _ = report_tx.send(report);
        Ok(main_file)
    })
    .await?;
    let skipped = report_rx
        .await
        .map(|report| {
            report
                .skipped
                .into_iter()
                .map(|(name, reason)| SkippedEntry {
                    name,
                    reason: reason.to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((
        StatusCode::CREATED,
        Json(ImportResponse { project, skipped }),
    ))
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    format: Option<String>,
}

/// Archive formats of `GET /projects/{id}/export`.
#[derive(Debug, Clone, Copy)]
enum ExportFormat {
    Zip,
    SevenZ,
}

impl ExportFormat {
    fn parse(format: Option<&str>) -> Result<Self, ApiError> {
        match format.map(str::trim) {
            None | Some("" | "zip") => Ok(Self::Zip),
            Some("7z") => Ok(Self::SevenZ),
            Some(other) => Err(ApiError::bad_request(format!(
                "unsupported format `{other}`: use zip or 7z"
            ))),
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Zip => "application/zip",
            Self::SevenZ => "application/x-7z-compressed",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::SevenZ => "7z",
        }
    }
}

async fn export(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let format = ExportFormat::parse(query.format.as_deref())?;
    let project = projects::get(&state, &id).await?;
    let root = state.paths.project_dir(&id);
    let temp = tempfile::tempfile_in(&state.paths.tmp).map_err(ApiError::internal)?;
    let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File, ApiError> {
        let mut file = match format {
            ExportFormat::Zip => export_zip(&root, temp)?,
            ExportFormat::SevenZ => export_7z(&root, temp)?,
        };
        std::io::Seek::rewind(&mut file).map_err(ApiError::internal)?;
        Ok(file)
    })
    .await??;
    let file = tokio::fs::File::from_std(file);
    let length = file.metadata().await.map_err(ApiError::internal)?.len();
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    Ok((
        [
            (header::CONTENT_TYPE, format.content_type().to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
            (
                header::CONTENT_DISPOSITION,
                content_disposition(&format!("{}.{}", project.name, format.extension())),
            ),
        ],
        body,
    )
        .into_response())
}

/// `attachment` header with an ASCII fallback and the UTF-8 name (RFC 6266).
pub(crate) fn content_disposition(file_name: &str) -> String {
    let ascii: String = file_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " ._-()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = file_name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b".-_".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::content_disposition;

    #[test]
    fn content_disposition_is_safe() {
        assert_eq!(
            content_disposition("Bài báo \"x\".zip"),
            "attachment; filename=\"B_i b_o _x_.zip\"; \
             filename*=UTF-8''B%C3%A0i%20b%C3%A1o%20%22x%22.zip"
        );
    }
}
