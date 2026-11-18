mod auth;
mod chat;
mod cli;
mod compiler;
mod completion;
mod db;
mod error;
mod events;
mod history;
mod projects;
mod routes;
mod settings;
mod state;
mod synctex_cache;

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Context;
use clap::Parser;
use openprism_core::engines::{DetectOptions, detect_engines};
use tokio::net::TcpListener;
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
    let data_dir = args.resolve_data_dir()?;
    tokio::fs::create_dir_all(&data_dir)
        .await
        .with_context(|| format!("cannot create data dir {}", data_dir.display()))?;

    let token = AccessToken::resolve(&data_dir).await?;
    if let Some(Command::PrintToken) = args.command {
        println!("{}", token.expose());
        return Ok(());
    }

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
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    tracing::info!(%addr, data_dir = %state.paths.root.display(), "open-prism-api listening");
    tracing::info!("sign in with the token from `open-prism-api print-token`");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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
