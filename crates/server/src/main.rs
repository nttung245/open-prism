mod auth;
mod cli;
mod error;
mod routes;
mod state;

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

    let app = routes::router(AppState::new(token, engines));

    // Never bind beyond loopback: the Next.js server is the only public entry point.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, args.port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    tracing::info!(%addr, data_dir = %data_dir.display(), "open-prism-api listening");
    tracing::info!("sign in with the token from `open-prism-api print-token`");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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
