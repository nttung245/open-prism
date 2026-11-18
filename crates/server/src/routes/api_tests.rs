//! End-to-end tests of the project, file and compile API against a real data directory.

use std::io::Write;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use openprism_core::engines::{EngineInfo, EngineKind, EngineRuntime};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::router;
use crate::auth::AccessToken;
use crate::state::AppState;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct TestApp {
    router: Router,
    state: AppState,
    _dir: tempfile::TempDir,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

impl TestApp {
    async fn new(engines: Vec<EngineInfo>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(
            &dir.path().join("data"),
            AccessToken::new(TOKEN).unwrap(),
            engines,
        )
        .await
        .unwrap();
        Self {
            router: router(state.clone()),
            state,
            _dir: dir,
        }
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body,
        }
    }

    fn request(method: Method, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> Reply {
        let builder = Self::request(method, uri);
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        };
        self.send(request.unwrap()).await
    }

    async fn create_project(&self, name: &str) -> String {
        let reply = self
            .call(Method::POST, "/api/projects", Some(json!({ "name": name })))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED);
        reply.json()["id"].as_str().unwrap().to_owned()
    }
}

fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "openprism-test-boundary";
    let mut body = Vec::new();
    for (name, file_name, data) in parts {
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\""
        )
        .unwrap();
        if let Some(file_name) = file_name {
            write!(body, "; filename=\"{file_name}\"").unwrap();
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{boundary}--\r\n").unwrap();
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[tokio::test]
async fn project_lifecycle() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("  My paper ").await;

    let list = app.call(Method::GET, "/api/projects", None).await.json();
    assert_eq!(list[0]["name"], "My paper");
    assert_eq!(list[0]["mainFile"], "main.tex");
    assert_eq!(list[0]["engine"], "auto");

    let uri = format!("/api/projects/{id}");
    let updated = app
        .call(
            Method::PATCH,
            &uri,
            Some(json!({ "name": "Renamed", "mainFile": "src/paper.tex", "engine": "xelatex" })),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK);
    assert_eq!(updated.json()["mainFile"], "src/paper.tex");
    assert_eq!(updated.json()["engine"], "xelatex");

    let bad = app
        .call(Method::PATCH, &uri, Some(json!({ "mainFile": "../x.tex" })))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let bad = app
        .call(Method::POST, "/api/projects", Some(json!({ "name": "  " })))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    let project_dir = app.state.paths.project_dir(&id);
    assert!(project_dir.join("main.tex").is_file());
    assert_eq!(
        app.call(Method::DELETE, &uri, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.call(Method::GET, &uri, None).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(!project_dir.exists());
}

#[tokio::test]
async fn unknown_and_malformed_project_ids_are_not_found() {
    let app = TestApp::new(Vec::new()).await;
    for uri in [
        "/api/projects/0190f1a2-0000-7000-8000-000000000000",
        "/api/projects/not-a-uuid/tree",
        "/api/projects/..%2F..%2Fetc/tree",
    ] {
        assert_eq!(
            app.call(Method::GET, uri, None).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn files_are_versioned() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Files").await;
    let uri = format!("/api/projects/{id}/files/main.tex");

    let read = app.call(Method::GET, &uri, None).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(
        read.headers[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    assert!(String::from_utf8_lossy(&read.body).contains("\\documentclass{article}"));
    let etag = read.headers[header::ETAG].to_str().unwrap().to_owned();

    let write = |if_match: &str, body: &'static str| {
        TestApp::request(Method::PUT, &uri)
            .header(header::IF_MATCH, if_match)
            .body(Body::from(body))
            .unwrap()
    };
    let saved = app.send(write(&etag, "new content")).await;
    assert_eq!(saved.status, StatusCode::OK);
    let new_etag = saved.headers[header::ETAG].to_str().unwrap().to_owned();
    assert_ne!(new_etag, etag);
    assert_eq!(
        new_etag,
        format!("\"{}\"", saved.json()["version"].as_str().unwrap())
    );

    let stale = app.send(write(&etag, "lost update")).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(stale.json()["error"]["code"], "version_conflict");
    assert_eq!(stale.headers[header::ETAG], new_etag.as_str());
    assert_eq!(app.call(Method::GET, &uri, None).await.body, "new content");

    let create_only = TestApp::request(Method::PUT, &uri)
        .header(header::IF_NONE_MATCH, "*")
        .body(Body::from("x"))
        .unwrap();
    assert_eq!(app.send(create_only).await.status, StatusCode::CONFLICT);

    let nested = format!("/api/projects/{id}/files/chapters/intro.tex");
    let created = app
        .send(
            TestApp::request(Method::PUT, &nested)
                .body(Body::from("Intro"))
                .unwrap(),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK);
    let tree = app
        .call(Method::GET, &format!("/api/projects/{id}/tree"), None)
        .await
        .json();
    let paths: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["chapters", "chapters/intro.tex", "main.tex"]);
}

#[tokio::test]
async fn project_files_cannot_escape_the_project() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Escape").await;
    let secret = app.state.paths.root.join("secret.txt");
    std::fs::write(&secret, "secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret, app.state.paths.project_dir(&id).join("link.txt")).unwrap();

    for path in [
        "..%2F..%2Fsecret.txt",
        "a/%2e%2e/%2e%2e/%2e%2e/secret.txt",
        "%2Fetc%2Fpasswd",
        "a%5C..%5C..%5Csecret.txt",
        "link.txt",
    ] {
        let uri = format!("/api/projects/{id}/files/{path}");
        let read = app.call(Method::GET, &uri, None).await;
        assert!(
            read.status == StatusCode::BAD_REQUEST || read.status == StatusCode::NOT_FOUND,
            "{path}: {}",
            read.status
        );
        assert!(!String::from_utf8_lossy(&read.body).contains("secret\""));
        let write = app
            .send(
                TestApp::request(Method::PUT, &uri)
                    .body(Body::from("pwned"))
                    .unwrap(),
            )
            .await;
        assert_ne!(write.status, StatusCode::OK, "{path}");
    }
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret");

    let rename = app
        .call(
            Method::POST,
            &format!("/api/projects/{id}/file-ops"),
            Some(json!({ "op": "rename", "from": "main.tex", "to": "../main.tex" })),
        )
        .await;
    assert_eq!(rename.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn file_operations_and_uploads() {
    let app = TestApp::new(Vec::new()).await;
    let id = app.create_project("Ops").await;
    let ops = format!("/api/projects/{id}/file-ops");
    for op in [
        json!({ "op": "mkdir", "path": "figures" }),
        json!({ "op": "createFile", "path": "figures/notes.tex" }),
        json!({ "op": "rename", "from": "figures/notes.tex", "to": "notes.tex" }),
    ] {
        let reply = app.call(Method::POST, &ops, Some(op.clone())).await;
        assert_eq!(
            reply.status,
            StatusCode::NO_CONTENT,
            "{op}: {:?}",
            reply.json()
        );
    }
    let duplicate = app
        .call(
            Method::POST,
            &ops,
            Some(json!({ "op": "createFile", "path": "notes.tex" })),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);

    let (content_type, body) = multipart(&[
        ("file", Some("plot.png"), b"\x89PNG fake"),
        ("file", Some("C:\\Users\\me\\data.csv"), b"a,b"),
    ]);
    let upload = app
        .send(
            TestApp::request(
                Method::POST,
                &format!("/api/projects/{id}/upload?dir=figures"),
            )
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap(),
        )
        .await;
    assert_eq!(upload.status, StatusCode::OK, "{:?}", upload.json());
    assert_eq!(
        upload.json()["paths"],
        json!(["figures/plot.png", "figures/data.csv"])
    );

    let image = app
        .call(
            Method::GET,
            &format!("/api/projects/{id}/files/figures/plot.png"),
            None,
        )
        .await;
    assert_eq!(image.headers[header::CONTENT_TYPE], "image/png");
    assert!(
        image.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .starts_with("sandbox")
    );

    let delete = app
        .call(
            Method::POST,
            &ops,
            Some(json!({ "op": "delete", "path": "figures" })),
        )
        .await;
    assert_eq!(delete.status, StatusCode::NO_CONTENT);
    assert!(!app.state.paths.project_dir(&id).join("figures").exists());
}

#[tokio::test]
async fn import_and_export_zip() {
    let app = TestApp::new(Vec::new()).await;
    let archive = zip_of(&[
        (
            "thesis/thesis.tex",
            b"\\documentclass{book}\n\\input{ch/one}",
        ),
        ("thesis/ch/one.tex", b"Chapter one"),
        ("thesis/../evil.tex", b"x"),
        ("__MACOSX/thesis/._thesis.tex", b"junk"),
    ]);
    let (content_type, body) = multipart(&[("file", Some("My Thesis.zip"), &archive)]);
    let reply = app
        .send(
            TestApp::request(Method::POST, "/api/projects/import")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.json());
    let imported = reply.json();
    assert_eq!(imported["project"]["name"], "My Thesis");
    assert_eq!(imported["project"]["mainFile"], "thesis.tex");
    assert_eq!(imported["skipped"].as_array().unwrap().len(), 2);
    let id = imported["project"]["id"].as_str().unwrap();
    assert!(!app.state.paths.projects.join("evil.tex").exists());

    let export = app
        .call(Method::GET, &format!("/api/projects/{id}/export"), None)
        .await;
    assert_eq!(export.status, StatusCode::OK);
    assert!(
        export.headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("My Thesis.zip")
    );
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(export.body.to_vec())).unwrap();
    let mut names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["ch/", "ch/one.tex", "thesis.tex"]);

    let (content_type, body) = multipart(&[("file", Some("x.zip"), b"not a zip")]);
    let reply = app
        .send(
            TestApp::request(Method::POST, "/api/projects/import")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        app.call(Method::GET, "/api/projects", None)
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// A fake `latexmk` that writes a PDF, SyncTeX data and a log into `-outdir`.
///
/// Written by a child process so this multi-threaded test binary never holds the script open
/// for writing while another test forks (that makes exec fail with "Text file busy").
#[cfg(unix)]
fn fake_latexmk(dir: &std::path::Path) -> EngineInfo {
    use std::process::{Command, Stdio};
    let path = dir.join("latexmk");
    let script = "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in -outdir=*) out=\"${a#-outdir=}\";; esac; done\n\
         sleep 0.2\nprintf '%%PDF-1.5 fake pdf' > \"$out/main.pdf\"\n\
         printf 'gz' > \"$out/main.synctex.gz\"\nprintf 'This is pdfTeX\\n' > \"$out/main.log\"\n";
    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    EngineInfo {
        kind: EngineKind::Latexmk,
        runtime: EngineRuntime::Native {
            path: path.display().to_string(),
        },
        version: None,
    }
}

/// Reads SSE frames until one of type `compileFinished` arrives.
async fn wait_for_finish(body: Body) -> Value {
    let mut body = body;
    let mut buffer = String::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), body.frame())
            .await
            .expect("no compileFinished event in time")
            .unwrap()
            .unwrap();
        if let Ok(data) = frame.into_data() {
            buffer.push_str(&String::from_utf8_lossy(&data));
        }
        while let Some(end) = buffer.find("\n\n") {
            let event: String = buffer.drain(..end + 2).collect();
            if let Some(json) = event.lines().find_map(|line| line.strip_prefix("data: ")) {
                let value: Value = serde_json::from_str(json).unwrap();
                if value["type"] == "compileFinished" {
                    return value["result"].clone();
                }
            }
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn compile_runs_in_the_background_and_serves_outputs() {
    let bin = tempfile::tempdir().unwrap();
    let app = TestApp::new(vec![fake_latexmk(bin.path())]).await;
    let id = app.create_project("Compile").await;

    let events = app
        .router
        .clone()
        .oneshot(
            TestApp::request(Method::GET, &format!("/api/projects/{id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(events.headers()[header::CONTENT_TYPE], "text/event-stream");

    assert_eq!(
        app.call(
            Method::GET,
            &format!("/api/projects/{id}/compile/latest"),
            None
        )
        .await
        .json(),
        Value::Null
    );
    let started = app
        .call(Method::POST, &format!("/api/projects/{id}/compile"), None)
        .await;
    assert_eq!(started.status, StatusCode::ACCEPTED);
    let run_id = started.json()["runId"].as_str().unwrap().to_owned();

    let result = wait_for_finish(events.into_body()).await;
    assert_eq!(result["runId"], run_id.as_str());
    assert_eq!(result["status"], "success", "{result}");
    assert_eq!(result["program"], "pdflatex");
    assert_eq!(result["runtime"], "native");
    assert_eq!(result["hasPdf"], true);
    assert_eq!(result["hasSynctex"], true);

    let latest = app
        .call(
            Method::GET,
            &format!("/api/projects/{id}/compile/latest"),
            None,
        )
        .await
        .json();
    assert_eq!(latest["runId"], run_id.as_str());

    let pdf_uri = format!("/api/projects/{id}/runs/{run_id}/output.pdf");
    let pdf = app.call(Method::GET, &pdf_uri, None).await;
    assert_eq!(pdf.status, StatusCode::OK);
    assert_eq!(pdf.headers[header::CONTENT_TYPE], "application/pdf");
    assert!(
        pdf.headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
    assert_eq!(pdf.body, "%PDF-1.5 fake pdf");
    let range = app
        .send(
            TestApp::request(Method::GET, &pdf_uri)
                .header(header::RANGE, "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(range.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.body, "%PDF");

    for bad in ["../../secret", "main.tex", "output.pdf%2F.."] {
        let uri = format!("/api/projects/{id}/runs/{run_id}/{bad}");
        assert_eq!(
            app.call(Method::GET, &uri, None).await.status,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }
    let other_run =
        format!("/api/projects/{id}/runs/0190f1a2-0000-7000-8000-000000000000/output.pdf");
    assert_eq!(
        app.call(Method::GET, &other_run, None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_new_compile_cancels_the_running_one() {
    let bin = tempfile::tempdir().unwrap();
    let app = TestApp::new(vec![fake_latexmk(bin.path())]).await;
    let id = app.create_project("Twice").await;
    let uri = format!("/api/projects/{id}/compile");
    let first = app.call(Method::POST, &uri, None).await.json()["runId"].clone();
    let second = app.call(Method::POST, &uri, None).await.json()["runId"].clone();

    let result_of = |run: Value| {
        let uri = format!("/api/projects/{id}/compile/{}", run.as_str().unwrap());
        let app = &app;
        async move {
            for _ in 0..100 {
                let reply = app.call(Method::GET, &uri, None).await;
                if reply.status == StatusCode::OK {
                    return reply.json();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("{uri} never finished");
        }
    };
    assert_eq!(result_of(first).await["status"], "cancelled");
    assert_eq!(result_of(second).await["status"], "success");
}
