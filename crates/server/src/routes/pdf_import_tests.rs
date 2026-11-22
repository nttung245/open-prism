//! The PDF import job end to end: a PDF built here, a model faked by wiremock that answers by
//! prompt, and a fake `latexmk`.

#![cfg(unix)]

use std::time::Duration;

use lopdf::{Document, Object, Stream, dictionary};
use openprism_ai::provider::ProviderKind;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::api_tests::{TestApp, fake_latexmk_running};
use crate::jobs::pdf_import::{KIND, PdfImportParams};
use crate::jobs::scheduler_tests::{finished, wait_for};
use crate::jobs::store::JobState;
use crate::settings::{self, CreateProviderRequest, ModelRef};

/// Letter pages; page 2 has a black rectangle at 0.1–0.5 of the width, 0.1–0.4 of the height.
fn pdf(pages: u32) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for n in 1..=pages {
        let drawing: &[u8] = if n == 2 {
            b"0 0 0 rg 61 475 245 238 re f"
        } else {
            b""
        };
        let content = doc.add_object(Stream::new(dictionary! {}, drawing.to_vec()));
        kids.push(Object::Reference(doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content,
        })));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Count" => i64::from(pages), "Kids" => kids },
        ),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn answer(text: &str) -> ResponseTemplate {
    let chunk = json!({"id": "x", "object": "chat.completion.chunk", "created": 1, "model": "m",
                       "choices": [{"index": 0, "delta": {"content": text}}]});
    let finish = json!({"id": "x", "object": "chat.completion.chunk", "created": 1, "model": "m",
                        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}});
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(format!(
            "data: {chunk}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        ))
}

const PREAMBLE: &str = "\\documentclass[11pt,letterpaper]{article}\n\\usepackage[margin=0.6in]{geometry}\n\
                        \\usepackage{minted}\n\\setcounter{secnumdepth}{0}\n\\begin{document}\n";

/// Mounts the answers of the fake model, by what the request asks.
async fn model(server: &MockServer, page_bodies: &[&str]) {
    let mount = |needle: String, response: ResponseTemplate| {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains(needle))
            .respond_with(response)
    };
    mount("first page of a PDF".into(), answer(PREAMBLE))
        .mount(server)
        .await;
    for (n, body) in page_bodies.iter().enumerate() {
        mount(format!("Page {} of the PDF.", n + 1), answer(body))
            .mount(server)
            .await;
    }
}

async fn provider(app: &TestApp, server: &MockServer) -> ModelRef {
    let view = settings::create(
        &app.state,
        &crate::auth::Principal::Local,
        CreateProviderRequest {
            name: "Fake".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: Some(format!("{}/v1", server.uri())),
            api_key: Some("sk-test".into()),
            shared: None,
        },
    )
    .await
    .unwrap();
    ModelRef {
        provider_id: view.id,
        model: "fake-vision".into(),
        reasoning: None,
    }
}

/// A fake latexmk that fails with an error on line 5 while main.tex says BROKEN.
const LATEXMK: &str = "if grep -q BROKEN main.tex; then\n\
     printf './main.tex:5: Missing $ inserted.\\n' > \"$out/main.log\"; exit 12; fi\n\
     printf '%%PDF-1.5 fake' > \"$out/main.pdf\"\nprintf 'This is pdfTeX\\n' > \"$out/main.log\"\n";

struct Import {
    app: TestApp,
    server: MockServer,
    model: ModelRef,
    _bin: tempfile::TempDir,
    _stop: tokio_util::sync::DropGuard,
}

async fn setup(page_bodies: &[&str]) -> Import {
    let bin = tempfile::tempdir().unwrap();
    let app = TestApp::new(vec![fake_latexmk_running(bin.path(), LATEXMK)]).await;
    app.state.jobs.set_retry_delay(Duration::from_millis(10));
    let server = MockServer::start().await;
    model(&server, page_bodies).await;
    let model = provider(&app, &server).await;
    let stop = CancellationToken::new();
    tokio::spawn(crate::jobs::run(app.state.clone(), stop.clone()));
    Import {
        app,
        server,
        model,
        _bin: bin,
        _stop: stop.drop_guard(),
    }
}

impl Import {
    async fn start(&self, pages: Vec<u32>, pdf_pages: u32) -> String {
        self.start_as(None, &self.model, pages, pdf_pages).await
    }

    /// Starts an import as `user_id` (server mode), with `model`.
    async fn start_as(
        &self,
        user_id: Option<&str>,
        model: &ModelRef,
        pages: Vec<u32>,
        pdf_pages: u32,
    ) -> String {
        let params = PdfImportParams {
            name: "Imported".into(),
            file_name: "paper.pdf".into(),
            pages,
            model: model.clone(),
        };
        let bytes = pdf(pdf_pages);
        crate::jobs::create_prepared(
            &self.app.state,
            user_id,
            KIND,
            "Import paper.pdf",
            &serde_json::to_value(params).unwrap(),
            move |dir| {
                std::fs::write(dir.join("source.pdf"), bytes)
                    .map_err(crate::error::ApiError::internal)
            },
        )
        .await
        .unwrap()
        .id
    }

    fn project_file(&self, project: &str, file: &str) -> std::path::PathBuf {
        self.app.state.paths.project_dir(project).join(file)
    }

    async fn requests_with(&self, needle: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| String::from_utf8_lossy(&request.body).contains(needle))
            .count()
    }
}

/// The width in pixels of a PNG file.
fn png_width(png: &[u8]) -> u32 {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    u32::from_be_bytes(png[16..20].try_into().unwrap())
}

#[tokio::test]
async fn a_pdf_becomes_a_project() {
    let import = setup(&[
        "\\section*{Introduction}\\label{sec:intro}\nAs shown in~\\cite{x}, text that goes on",
        "on this page.\n\\begin{figure}[h]\\centering\\includegraphics[width=0.5\\linewidth]{box:0.1,0.1,0.5,0.4}\
         \\caption{A square.}\\end{figure}\n",
        "New paragraph. \\includegraphics{box:0.9,0.1,0.2,0.5}\n",
    ])
    .await;
    let job = import.start(vec![1, 2, 3], 3).await;
    let done = finished(&import.app.state, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!((done.steps_done, done.steps_total), (3, Some(3)));
    let project = done.project_id.unwrap();

    let main = std::fs::read_to_string(import.project_file(&project, "main.tex")).unwrap();
    assert!(
        main.starts_with("\\documentclass[11pt,letterpaper]{article}\n"),
        "{main}"
    );
    assert!(
        !main.contains("minted"),
        "packages outside the list are dropped"
    );
    assert_eq!(main.matches("\\begin{document}").count(), 1);
    assert!(
        !main.contains("\\cite") && !main.contains("\\label"),
        "{main}"
    );
    assert!(
        main.contains("As shown in~[x], text that goes on%\n% --- page 2 ---\non this page."),
        "{main}"
    );
    assert!(
        main.contains("\\end{figure}\n\n% --- page 3 ---\nNew paragraph."),
        "{main}"
    );
    assert!(
        main.contains("\\includegraphics[width=0.5\\linewidth]{figures/p2-1.png}"),
        "{main}"
    );
    assert!(
        main.contains("Figure (could not be cut out)"),
        "the impossible box is a placeholder"
    );

    // The picture, cut out at 250 DPI with a 3% margin: 0.46 of 8.5 in.
    let picture = std::fs::read(import.project_file(&project, "figures/p2-1.png")).unwrap();
    let width = png_width(&picture);
    assert!(f64::from(width) / (0.46 * 8.5) >= 200.0, "{width} px");

    let snapshot: String = sqlx::query_scalar("SELECT summary FROM snapshots WHERE project_id = ?")
        .bind(&project)
        .fetch_one(&import.app.state.db)
        .await
        .unwrap();
    assert_eq!(snapshot, "Imported from PDF");
    // The project is compiled when it is created.
    let compiled = wait_for(&import.app.state, &job, |_| {
        import.app.state.paths.runs_dir(&project).exists()
    })
    .await;
    assert_eq!(compiled.state, JobState::Succeeded);
    assert_eq!(import.requests_with("first page of a PDF").await, 1);
}

#[tokio::test]
async fn the_project_of_an_import_belongs_to_whoever_started_it() {
    let import = setup(&["Some text.\n"]).await;
    let minh = crate::users::create(
        &import.app.state.db,
        "minh",
        "Minh",
        crate::users::UserRole::User,
        0,
    )
    .await
    .unwrap();
    let job = import
        .start_as(Some(&minh.id), &import.model, vec![1], 1)
        .await;
    let done = finished(&import.app.state, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    let project = done.project_id.unwrap();
    assert_eq!(
        crate::members::role_of(&import.app.state.db, &project, &minh.id)
            .await
            .unwrap(),
        Some(crate::members::ProjectRole::Owner)
    );
}

/// The keys of the requests `server` received.
async fn keys(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            request.headers["authorization"]
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn a_job_uses_the_key_of_whoever_started_it() {
    let import = setup(&["Some text.\n"]).await;
    let state = &import.app.state;
    let account = |name: &'static str| async move {
        crate::users::create(&state.db, name, name, crate::users::UserRole::User, 0)
            .await
            .unwrap()
    };
    let lan = account("lan").await;
    let minh = account("minh").await;
    // Lan's own provider, on its own fake server.
    let lan_server = MockServer::start().await;
    model(&lan_server, &["Some text.\n"]).await;
    let view = settings::create(
        state,
        &crate::auth::Principal::User(lan.clone()),
        CreateProviderRequest {
            name: "Lan's".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: Some(format!("{}/v1", lan_server.uri())),
            api_key: Some("sk-lan".into()),
            shared: None,
        },
    )
    .await
    .unwrap();
    assert!(!view.shared);
    let lans_model = ModelRef {
        provider_id: view.id,
        model: "fake-vision".into(),
        reasoning: None,
    };

    // Lan's import asks with Lan's key, and only hers.
    let job = import
        .start_as(Some(&lan.id), &lans_model, vec![1], 1)
        .await;
    let done = finished(state, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    let sent = keys(&lan_server).await;
    assert!(!sent.is_empty());
    assert!(sent.iter().all(|key| key == "Bearer sk-lan"), "{sent:?}");
    assert!(keys(&import.server).await.is_empty());

    // A job of Minh's naming Lan's provider never reaches it: with no model of his own, it fails
    // and says what to do.
    let job = import
        .start_as(Some(&minh.id), &lans_model, vec![1], 1)
        .await;
    let failed = finished(state, &job).await;
    assert_eq!(failed.state, JobState::Failed, "{failed:?}");
    assert!(
        failed
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("choose a model for Agent"),
        "{failed:?}"
    );
    assert_eq!(keys(&lan_server).await.len(), sent.len());

    // Once Minh has a model (here the shared default), running the job again works.
    settings::set_default_roles(
        state,
        settings::Roles {
            agent: Some(import.model.clone()),
            fast: None,
        },
    )
    .await
    .unwrap();
    crate::jobs::retry(state, &job).await.unwrap();
    let retried = finished(state, &job).await;
    assert_eq!(retried.state, JobState::Succeeded, "{retried:?}");
    let shared = keys(&import.server).await;
    assert!(
        !shared.is_empty() && shared.iter().all(|key| key == "Bearer sk-test"),
        "{shared:?}"
    );
    assert_eq!(keys(&lan_server).await.len(), sent.len());
}

#[tokio::test]
async fn compile_errors_are_fixed_by_the_agent() {
    let import = setup(&["Hello \\textbf{BROKEN}_{x} world.\n"]).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("You fix LaTeX compile errors"))
        .respond_with(answer(
            "Fixed.\nmain.tex\n<<<<<<< SEARCH\nHello \\textbf{BROKEN}_{x} world.\n=======\nHello $\\textbf{fixed}_{x}$ world.\n>>>>>>> REPLACE\n",
        ))
        .mount(&import.server)
        .await;
    let job = import.start(vec![1], 1).await;
    let done = finished(&import.app.state, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    let main = std::fs::read_to_string(import.project_file(&done.project_id.unwrap(), "main.tex"))
        .unwrap();
    assert!(
        main.contains("Hello $\\textbf{fixed}_{x}$ world."),
        "{main}"
    );
    let fix_request = import
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| String::from_utf8_lossy(&r.body).contains("You fix LaTeX compile errors"))
        .unwrap();
    let body: Value = serde_json::from_slice(&fix_request.body).unwrap();
    let asked = body.to_string();
    assert!(
        asked.contains("Missing $ inserted") && asked.contains("BROKEN"),
        "the error and its lines"
    );
}

#[tokio::test]
async fn errors_left_after_two_rounds_still_make_a_project() {
    let import = setup(&["Hello \\textbf{BROKEN}_{x} world.\n"]).await;
    // A fix that does not fix.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("You fix LaTeX compile errors"))
        .respond_with(answer(
            "main.tex\n<<<<<<< SEARCH\nworld.\n=======\nworld!\n>>>>>>> REPLACE\n",
        ))
        .mount(&import.server)
        .await;
    let job = import.start(vec![1], 1).await;
    let done = finished(&import.app.state, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!(
        import.requests_with("You fix LaTeX compile errors").await,
        2
    );
    let main = std::fs::read_to_string(import.project_file(&done.project_id.unwrap(), "main.tex"))
        .unwrap();
    assert!(main.contains("BROKEN"));
}

#[tokio::test]
async fn a_cancelled_import_makes_no_project() {
    let import = setup(&[]).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("You turn one page"))
        .respond_with(answer("Slow page.").set_delay(Duration::from_secs(5)))
        .mount(&import.server)
        .await;
    let job = import.start(vec![1, 2, 3, 4], 4).await;
    wait_for(&import.app.state, &job, |job| job.steps_total.is_some()).await;
    crate::jobs::cancel(&import.app.state, &job).await.unwrap();
    let done = finished(&import.app.state, &job).await;
    assert_eq!(done.state, JobState::Cancelled);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        std::fs::read_dir(&import.app.state.paths.projects)
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn a_wrong_key_fails_and_a_retry_copies_only_the_pages_left() {
    let import = setup(&["Page one.\n", "Page two.\n"]).await;
    // Page 3 is refused once, late enough for pages 1 and 2 (rendered in a debug build) to be
    // done, then answered.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("Page 3 of the PDF."))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "Incorrect API key provided"}}))
                .set_delay(Duration::from_secs(2)),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&import.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("Page 3 of the PDF."))
        .respond_with(answer("Page three.\n"))
        .with_priority(2)
        .mount(&import.server)
        .await;
    let job = import.start(vec![1, 2, 3], 3).await;
    let failed = finished(&import.app.state, &job).await;
    assert_eq!(failed.state, JobState::Failed);
    assert!(failed.error.unwrap().contains("Incorrect API key provided"));
    assert_eq!(failed.steps_done, 2);

    crate::jobs::retry(&import.app.state, &job).await.unwrap();
    let done = finished(&import.app.state, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!(import.requests_with("Page 1 of the PDF.").await, 1);
    assert_eq!(import.requests_with("Page 2 of the PDF.").await, 1);
    assert_eq!(import.requests_with("Page 3 of the PDF.").await, 2);
    assert_eq!(
        import.requests_with("first page of a PDF").await,
        1,
        "not planned again"
    );
    let main = std::fs::read_to_string(import.project_file(&done.project_id.unwrap(), "main.tex"))
        .unwrap();
    assert!(main.contains("Page one.") && main.contains("Page three."));
}

#[tokio::test]
async fn an_empty_answer_is_asked_again() {
    // A reasoning model that spent its budget thinking answers nothing: once for page 2.
    let import = setup(&[]).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("Page 2 of the PDF."))
        .respond_with(answer(""))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&import.server)
        .await;
    for (n, text) in [(1, "Page one.\n"), (2, "Page two.\n")] {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains(format!("Page {n} of the PDF.")))
            .respond_with(answer(text))
            .with_priority(2)
            .mount(&import.server)
            .await;
    }
    let job = import.start(vec![1, 2], 2).await;
    let done = finished(&import.app.state, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!(import.requests_with("Page 2 of the PDF.").await, 2);
    let main = std::fs::read_to_string(import.project_file(&done.project_id.unwrap(), "main.tex"))
        .unwrap();
    assert!(main.contains("Page two."), "{main}");
    // Copying is asked with reasoning off.
    let requests = import.server.received_requests().await.unwrap();
    let page = requests
        .iter()
        .find(|r| String::from_utf8_lossy(&r.body).contains("Page 1 of the PDF."))
        .unwrap();
    let body: Value = serde_json::from_slice(&page.body).unwrap();
    assert!(
        body.get("reasoning_effort")
            .is_none_or(|effort| effort == "none" || effort == "minimal"),
        "{body}"
    );
}

mod route {
    use axum::body::Body;
    use axum::http::{Method, StatusCode, header};

    use super::*;
    use crate::settings::Roles;

    async fn app_with_agent() -> (TestApp, MockServer) {
        let app = TestApp::new(Vec::new()).await;
        let server = MockServer::start().await;
        let model = provider(&app, &server).await;
        settings::set_default_roles(
            &app.state,
            Roles {
                agent: Some(model),
                fast: None,
            },
        )
        .await
        .unwrap();
        (app, server)
    }

    /// A multipart field: its name, file name, and bytes.
    type Part<'a> = (&'a str, Option<&'a str>, &'a [u8]);

    async fn post(app: &TestApp, parts: &[Part<'_>]) -> super::super::api_tests::Reply {
        let (content_type, body) = super::super::api_tests::multipart(parts);
        app.send(
            TestApp::request(Method::POST, "/api/imports/pdf")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }

    async fn no_jobs(app: &TestApp) -> bool {
        crate::jobs::store::list(&app.state.db, crate::db::now_ms(), None)
            .await
            .unwrap()
            .is_empty()
    }

    #[tokio::test]
    async fn queues_an_import_of_every_page_named_after_the_file() {
        let (app, _server) = app_with_agent().await;
        let reply = post(&app, &[("file", Some("paper.pdf"), &pdf(3))]).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{:?}", reply.json());
        let job = reply.json();
        assert_eq!(job["kind"], "pdfImport");
        assert_eq!(job["title"], "Import paper.pdf");
        assert_eq!(job["state"], "queued");
        let row = crate::jobs::store::get(&app.state.db, job["id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        let params: PdfImportParams = serde_json::from_value(row.params).unwrap();
        assert_eq!(
            (params.name.as_str(), params.pages.as_slice()),
            ("paper", &[1, 2, 3][..])
        );
        assert!(
            app.state
                .paths
                .job_dir(job["id"].as_str().unwrap())
                .join("source.pdf")
                .is_file()
        );

        let reply = post(
            &app,
            &[
                ("file", Some("thesis.pdf"), &pdf(12)),
                ("name", None, b"My thesis"),
                ("pages", None, b"2-4, 9"),
            ],
        )
        .await;
        assert_eq!(reply.status, StatusCode::ACCEPTED);
        let row = crate::jobs::store::get(&app.state.db, reply.json()["id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        let params: PdfImportParams = serde_json::from_value(row.params).unwrap();
        assert_eq!(
            (params.name.as_str(), params.pages.as_slice()),
            ("My thesis", &[2, 3, 4, 9][..])
        );
    }

    #[tokio::test]
    async fn refuses_without_an_agent_model() {
        let app = TestApp::new(Vec::new()).await;
        let reply = post(&app, &[("file", Some("paper.pdf"), &pdf(1))]).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert_eq!(reply.json()["error"]["code"], "no_model");
        assert!(
            reply.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("choose a model for Agent in Settings")
        );
        assert!(no_jobs(&app).await);
    }

    #[tokio::test]
    async fn refuses_files_and_pages_it_cannot_import() {
        let (app, _server) = app_with_agent().await;
        let cases: [(&[Part], &str); 4] = [
            (
                &[("file", Some("notes.pdf"), b"just some text, not a PDF")],
                "not a PDF Open Prism can read",
            ),
            (
                &[("file", Some("x.pdf"), b"%PDF-1.5 broken")],
                "not a PDF Open Prism can read",
            ),
            (&[("name", None, b"no file")], "missing the `file` field"),
            (
                &[("file", Some("p.pdf"), &pdf(12)), ("pages", None, b"10-14")],
                "there is no page 14: this PDF has pages 1\u{2013}12",
            ),
        ];
        for (parts, expected) in cases {
            let reply = post(&app, parts).await;
            assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{expected}");
            let message = reply.json()["error"]["message"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(message.contains(expected), "{message}");
        }
        let reply = post(&app, &[("file", Some("long.pdf"), &pdf(301))]).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert!(
            reply.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("at most 300 pages")
        );
        assert!(no_jobs(&app).await);
        assert_eq!(std::fs::read_dir(&app.state.paths.jobs).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn refuses_a_pdf_over_200_mb() {
        let (app, _server) = app_with_agent().await;
        let boundary = "openprism-test-boundary";
        let head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"huge.pdf\"\r\n\r\n%PDF-1.5\n"
        );
        let tail = format!("\r\n--{boundary}--\r\n");
        // 201 MB, streamed: never held in memory at once.
        let chunk = axum::body::Bytes::from(vec![b'0'; 1024 * 1024]);
        let chunks = std::iter::once(axum::body::Bytes::from(head))
            .chain(std::iter::repeat_n(chunk, 201))
            .chain(std::iter::once(axum::body::Bytes::from(tail)))
            .map(Ok::<_, std::io::Error>);
        let reply = app
            .send(
                TestApp::request(Method::POST, "/api/imports/pdf")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            reply.status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{:?}",
            reply.json()
        );
        assert!(no_jobs(&app).await);
    }
}
