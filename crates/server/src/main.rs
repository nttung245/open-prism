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
mod jobs;
mod links;
mod login_limit;
mod mode;
mod passwords;
mod projects;
mod routes;
mod sessions;
mod settings;
mod state;
mod synctex_cache;
mod user_cli;
mod users;
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
    // Check the mode flags before anything else: a server-mode mistake must stop here.
    let mode = args.mode().map_err(anyhow::Error::msg)?;
    let serving = matches!(args.command, None | Some(Command::Serve(_)));
    if let Some(server) = mode.server().filter(|_| serving) {
        tracing::info!(public_url = %server.public_url, "server mode: accounts and passwords");
        if server.insecure_http {
            tracing::warn!(
                public_url = %server.public_url,
                "--insecure-http: passwords and session cookies travel unencrypted. Use this only \
                 for a test on a network you trust, never on the Internet"
            );
        }
    }
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
    if let Some(Command::User { action }) = &args.command {
        println!("{}", user_cli::run(&data_dir, &mode, action).await?);
        return Ok(());
    }
    let sign_in_token = token.expose().to_owned();

    // `serve --web --open` again while Open Prism runs (a second click on its menu entry):
    // show the running one instead of failing on a busy port.
    if let Some(options) = &web_options
        && options.open
        && (web::port_in_use(args.port) || web::port_in_use(options.port))
        && web::is_open_prism(options.port).await
    {
        let link = match mode.server() {
            Some(server) => server.public_url.link("/login"),
            None => web::sign_in_link(
                &format!("http://127.0.0.1:{}", options.port),
                &sign_in_token,
            ),
        };
        println!("\n  Open Prism is already running. Open this link to sign in:\n  {link}\n");
        web::open_in_browser(&link).await;
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

    let state = AppState::open_in(mode.clone(), &data_dir, token, engines).await?;
    let _watcher = watch_files(&state)?;
    // Background jobs stop with the app; a job cut short resumes at the next start.
    let jobs_stop = tokio_util::sync::CancellationToken::new();
    let scheduler = tokio::spawn(jobs::run(state.clone(), jobs_stop.clone()));
    tokio::spawn({
        let stop = jobs_stop.clone();
        async move {
            shutdown_signal().await;
            stop.cancel();
        }
    });
    let served = serve(&args, state, web_options, web_dir, &sign_in_token).await;
    jobs_stop.cancel();
    if let Err(err) = scheduler.await {
        tracing::warn!(%err, "the background job scheduler stopped badly");
    }
    served
}

/// Serves the API (and, with `serve --web`, the web app) until Ctrl+C / SIGTERM / SIGHUP.
async fn serve(
    args: &cli::Args,
    state: AppState,
    web_options: Option<cli::WebOptions>,
    web_dir: Option<std::path::PathBuf>,
    sign_in_token: &str,
) -> anyhow::Result<()> {
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

    let (link, message) = startup_link(&state, sign_in_token).await?;
    let (Some(options), Some(dir)) = (web_options, web_dir) else {
        match state.mode.server() {
            Some(_) => tracing::info!(%link, "{message}"),
            None => tracing::info!("sign in with the token from `open-prism-api print-token`"),
        }
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        return Ok(());
    };

    web::check_web_port(options.host, options.port)?;
    tracing::info!(dir = %dir.display(), "starting the web app");
    if !options.host.is_loopback() {
        tracing::info!(
            "the web app listens on {}:{} (every address of this machine or container)",
            options.host,
            options.port
        );
    }
    let web = web::WebServer::next(options.node, dir, options.host, options.port, args.port);
    let link = match state.mode {
        mode::Mode::Single => web::sign_in_link(&web.url(), sign_in_token),
        mode::Mode::Server(_) => link,
    };
    run_with_web(listener, app, web, link, message, options.open).await
}

/// What to print once the web app is ready (`app-launch` spec): in single-user mode the
/// sign-in link with the token; in server mode the first-admin link while there is no account,
/// otherwise the sign-in page of the public address. Never the token in server mode.
async fn startup_link(
    state: &AppState,
    sign_in_token: &str,
) -> anyhow::Result<(String, &'static str)> {
    let Some(server) = state.mode.server() else {
        return Ok((
            sign_in_token.to_owned(),
            "Open Prism is running. Open this link to sign in:",
        ));
    };
    match links::first_admin(&state.db, db::now_ms()).await? {
        Some(token) => Ok((
            server.public_url.link(&format!("/setup/{token}")),
            "Open Prism is running in server mode with no account yet. Open this link to create \
             the first admin (it works once, for 24 hours):",
        )),
        None => Ok((
            server.public_url.link("/login"),
            "Open Prism is running in server mode. Sign in at:",
        )),
    }
}

/// Serves the API with the web app next to it, until Ctrl+C / SIGTERM / SIGHUP or until one of
/// them fails. The web app stops first, so it never sends requests to a closed API. With `open`,
/// the sign-in link opens in the browser once the web app is ready.
async fn run_with_web(
    listener: TcpListener,
    app: axum::Router,
    web: web::WebServer,
    link: String,
    message: &str,
    open: bool,
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
                    println!("\n  {message}\n  {link}\n");
                    if open {
                        let link = link.clone();
                        tokio::spawn(async move { web::open_in_browser(&link).await });
                    }
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

    // The terminal it runs in was closed (a window opened from the application menu).
    #[cfg(unix)]
    let hangup = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => tracing::error!(%err, "cannot listen for SIGHUP"),
        }
    };
    #[cfg(not(unix))]
    let hangup = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
        () = hangup => {},
    }
    tracing::info!("shutting down");
}
