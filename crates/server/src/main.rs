mod auth;
mod chat;
mod cli;
mod compiler;
mod completion;
mod db;
mod error;
mod events;
mod history;
mod image_latex;
mod images;
mod projects;
mod routes;
mod settings;
mod state;
mod synctex_cache;
mod web;

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Context;
use clap::Parser;
use openprism_core::engines::{DetectOptions, detect_engines};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::auth::AccessToken;
use crate::cli::Command;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info")),
        )
        .init();

    // Projects, the database and the token are private to the user running the server.
    #[cfg(unix)]
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));

    let args = cli::Args::parse();
    // `serve --web`: fail on a missing build or Node before starting anything.
    let web_options = args.web();
    let web_dir = match &web_options {
        Some(options) => {
            let dir = web::check_web_dir(&options.dir)?;
            let version = web::check_node(&options.node).await?;
            tracing::info!(node = %options.node.display(), version, "Node.js");
            Some(dir)
        }
        None => None,
    };

    let data_dir = args.resolve_data_dir()?;
    tokio::fs::create_dir_all(&data_dir)
        .await
        .with_context(|| format!("cannot create data dir {}", data_dir.display()))?;

    let token = AccessToken::resolve(&data_dir).await?;
    if let Some(Command::PrintToken) = args.command {
        println!("{}", token.expose());
        return Ok(());
    }
    let sign_in_token = token.expose().to_owned();

    let engines = detect_engines(&DetectOptions {
        search_path: None,
        docker_image: args.docker_image(),
    })
    .await;
    if engines.is_empty() {
        tracing::warn!(
            "no TeX engine found: pull the TeX Live image (`docker pull {}`) or install \
             latexmk / tectonic",
            args.tex_image
        );
    }
    for engine in &engines {
        tracing::info!(kind = ?engine.kind, runtime = ?engine.runtime, version = ?engine.version, "TeX engine");
    }

    let state = AppState::open(&data_dir, token, engines).await?;
    let _watcher = watch_files(&state)?;
    let app = routes::router(state.clone());

    // Never bind beyond loopback: the Next.js server is the only public entry point.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, args.port));
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => anyhow::bail!(
            "port {} on 127.0.0.1 is already in use (is another Open Prism still running?): \
             stop that program, or pass another port with --port",
            args.port
        ),
        Err(err) => return Err(err).with_context(|| format!("cannot listen on {addr}")),
    };
    tracing::info!(%addr, data_dir = %state.paths.root.display(), "open-prism-api listening");

    let (Some(options), Some(dir)) = (web_options, web_dir) else {
        tracing::info!("sign in with the token from `open-prism-api print-token`");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        return Ok(());
    };

    web::check_web_port(options.port)?;
    tracing::info!(dir = %dir.display(), "starting the web app");
    let web = web::WebServer::next(options.node, dir, options.port, args.port);
    let link = web::sign_in_link(&web.url(), &sign_in_token);
    run_with_web(listener, app, web, link).await
}

/// Serves the API with the web app next to it, until Ctrl+C / SIGTERM or until one of them
/// fails. The web app stops first, so it never sends requests to a closed API.
async fn run_with_web(
    listener: TcpListener,
    app: axum::Router,
    web: web::WebServer,
    link: String,
) -> anyhow::Result<()> {
    let api_stop = CancellationToken::new();
    let mut api = tokio::spawn({
        let stop = api_stop.clone();
        async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .map_err(anyhow::Error::from)
        }
    });
    let web_stop = CancellationToken::new();
    let (ready, mut readied) = oneshot::channel();
    let mut supervisor = tokio::spawn(web.supervise(web_stop.clone(), ready));

    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut announced = false;
    let exit = loop {
        tokio::select! {
            () = &mut signal => break Exit::Signal,
            result = &mut supervisor => break Exit::Web(joined(result)),
            result = &mut api => break Exit::Api(joined(result)),
            result = &mut readied, if !announced => {
                announced = true;
                if result.is_ok() {
                    println!("\n  Open Prism is running. Open this link to sign in:\n  {link}\n");
                }
            }
        }
    };

    match exit {
        Exit::Signal => {
            web_stop.cancel();
            let web = joined(supervisor.await);
            api_stop.cancel();
            web.and(joined(api.await))
        }
        Exit::Web(result) => {
            api_stop.cancel();
            let api = joined(api.await);
            result.and(api)
        }
        Exit::Api(result) => {
            web_stop.cancel();
            let _ = supervisor.await;
            result
        }
    }
}

/// What ended `run_with_web`.
enum Exit {
    Signal,
    Web(anyhow::Result<()>),
    Api(anyhow::Result<()>),
}

fn joined(result: Result<anyhow::Result<()>, tokio::task::JoinError>) -> anyhow::Result<()> {
    result
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
}

/// Forwards file-system changes in project directories to the projects' event streams.
fn watch_files(state: &AppState) -> anyhow::Result<openprism_core::watch::ProjectsWatcher> {
    let (watcher, mut changes) = openprism_core::watch::watch_projects(
        &state.paths.projects,
        std::time::Duration::from_millis(150),
    )
    .context("cannot watch the projects directory")?;
    let state = state.clone();
    tokio::spawn(async move {
        while let Some(batch) = changes.recv().await {
            for change in batch {
                state.events.publish(
                    &change.project_id,
                    events::ProjectEvent::FilesChanged {
                        paths: change.paths.into_iter().collect(),
                    },
                );
            }
        }
    });
    Ok(watcher)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "cannot listen for Ctrl+C");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => tracing::error!(%err, "cannot listen for SIGTERM"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
