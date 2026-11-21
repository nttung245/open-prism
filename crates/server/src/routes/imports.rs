//! Importing a PDF as a new project: checks the upload, then queues a background job
//! (`pdf-import` spec).

use std::io::Read;

use axum::extract::multipart::MultipartError;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use openprism_core::pdf_pages::{MAX_PDF_BYTES, parse_pages};
use openprism_core::pdf_render::PdfDocument;
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;
use crate::jobs::pdf_import::{KIND, PdfImportParams};
use crate::jobs::{self, store::JobView};
use crate::settings;
use crate::state::AppState;

/// Most pages imported at once: each page is a request to the Agent.
pub const MAX_PAGES: usize = 300;

pub fn routes() -> Router<AppState> {
    // The PDF plus the small fields around it.
    let limit = usize::try_from(MAX_PDF_BYTES).unwrap_or(usize::MAX) + 1024 * 1024;
    Router::new().route(
        "/imports/pdf",
        post(import_pdf).layer(DefaultBodyLimit::max(limit)),
    )
}

/// `multipart/form-data` with `file` (the PDF), and optionally `name` (of the new project) and
/// `pages` (such as `1-5, 8`; every page when absent).
async fn import_pdf(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<JobView>), ApiError> {
    let mut name: Option<String> = None;
    let mut pages: Option<String> = None;
    let mut upload: Option<(tempfile::NamedTempFile, Option<String>)> = None;
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("name") => name = Some(field.text().await.map_err(multipart_error)?),
            Some("pages") => pages = Some(field.text().await.map_err(multipart_error)?),
            Some("file") => {
                let file_name = field.file_name().map(str::to_owned);
                let temp = tempfile::NamedTempFile::new_in(&state.paths.tmp)
                    .map_err(ApiError::internal)?;
                let mut out = tokio::fs::File::from_std(temp.reopen().map_err(ApiError::internal)?);
                while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
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
            "missing the `file` field with the PDF",
        ));
    };
    let file_name = file_name.unwrap_or_else(|| "document.pdf".to_owned());

    // Checked once the upload is read: a reply sent while the browser is still sending the file
    // breaks the connection, and the page would show a network error instead of this message.
    let model = settings::roles(&state).await?.agent.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            // The page shows a link to Settings for this code.
            "no_model",
            "choose a model for Agent in Settings first: importing a PDF has the Agent read \
             every page",
        )
    })?;

    // A PDF Open Prism can read, and its number of pages.
    let path = temp.path().to_path_buf();
    let count = tokio::task::spawn_blocking(move || -> Result<u32, String> {
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|err| err.to_string())?;
        if !bytes.starts_with(b"%PDF-") {
            return Err("it does not start like a PDF".into());
        }
        PdfDocument::load(bytes)
            .map(|doc| doc.page_count())
            .map_err(|err| err.to_string())
    })
    .await?
    .map_err(|reason| {
        ApiError::bad_request(format!(
            "this file is not a PDF Open Prism can read ({reason})"
        ))
    })?;

    let spec = pages
        .as_deref()
        .map(str::trim)
        .filter(|spec| !spec.is_empty())
        .unwrap_or("1-");
    let pages = parse_pages(spec, count).map_err(|err| ApiError::bad_request(err.to_string()))?;
    if pages.len() > MAX_PAGES {
        return Err(ApiError::bad_request(format!(
            "import at most {MAX_PAGES} pages at a time; choose fewer pages (this PDF has {count})"
        )));
    }

    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let stem = file_name
                .strip_suffix(".pdf")
                .or_else(|| file_name.strip_suffix(".PDF"))
                .unwrap_or(&file_name);
            stem.to_owned()
        });
    let params = PdfImportParams {
        name,
        file_name: file_name.clone(),
        pages,
        model,
    };
    let params = serde_json::to_value(params).map_err(ApiError::internal)?;
    let job = jobs::create_prepared(
        &state,
        KIND,
        &format!("Import {file_name}"),
        &params,
        move |dir| {
            temp.persist(dir.join("source.pdf"))
                .map(|_| ())
                .map_err(|err| ApiError::internal(err.error))
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// A failed upload: 413 when it is too large, 400 otherwise.
fn multipart_error(err: MultipartError) -> ApiError {
    let status = err.status();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::new(
            status,
            "too_large",
            format!(
                "the PDF is larger than {} MB",
                MAX_PDF_BYTES / (1024 * 1024)
            ),
        );
    }
    ApiError::bad_request(err.body_text())
}
