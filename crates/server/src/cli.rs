use std::net::Ipv4Addr;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use openprism_core::engines::DEFAULT_DOCKER_IMAGE;

/// Open Prism API server. Listens on 127.0.0.1 only; the Next.js app is the public entry point.
#[derive(Debug, Parser)]
#[command(name = "open-prism-api", version, about)]
pub struct Args {
    /// Directory holding projects, build output, history, the database and the access token.
    /// Defaults to the platform data dir, e.g. ~/.local/share/open-prism.
    #[arg(long, env = "OPENPRISM_DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,

    /// TCP port of the API on 127.0.0.1.
    #[arg(long, env = "OPENPRISM_PORT", default_value_t = 8787, global = true)]
    pub port: u16,

    /// TeX Live Docker image used to compile when Docker is available ("none" disables it).
    /// It is never pulled automatically: run `docker pull <image>` once.
    #[arg(long, env = "OPENPRISM_TEX_IMAGE", default_value = DEFAULT_DOCKER_IMAGE, global = true)]
    pub tex_image: String,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the API server (default); with `--web`, run the web app next to it.
    Serve(ServeArgs),
    /// Print the access token used to sign in (creates it if needed).
    PrintToken,
}

#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// Also run the web app from this standalone Next.js build (`web/.next/standalone` after
    /// `just build`), on 127.0.0.1, and print a sign-in link once it is ready.
    #[arg(long, value_name = "DIR")]
    pub web: Option<PathBuf>,

    /// TCP port of the web app.
    #[arg(long, env = "OPENPRISM_WEB_PORT", default_value_t = 3737)]
    pub web_port: u16,

    /// Address the web app listens on: 127.0.0.1 (this computer only) or 0.0.0.0 (every
    /// address, for a container whose port is published on the host's 127.0.0.1). The API
    /// always listens on 127.0.0.1.
    #[arg(
        long,
        env = "OPENPRISM_WEB_HOST",
        default_value = "127.0.0.1",
        value_name = "ADDRESS",
        value_parser = web_host
    )]
    pub web_host: Ipv4Addr,

    /// Node.js (22 or newer) used to run the web app.
    #[arg(long, env = "OPENPRISM_NODE", default_value = "node")]
    pub node: PathBuf,

    /// Open the sign-in link in the default browser (`$BROWSER` when set) once the web app is
    /// ready. When Open Prism already runs on these ports, only open it and exit.
    #[arg(long, env = "OPENPRISM_OPEN")]
    pub open: bool,
}

/// `--web-host`: only loopback, or every address for a container.
fn web_host(value: &str) -> Result<Ipv4Addr, String> {
    match value.parse() {
        Ok(addr) if addr == Ipv4Addr::LOCALHOST || addr == Ipv4Addr::UNSPECIFIED => Ok(addr),
        _ => Err(
            "use 127.0.0.1 (this computer only) or 0.0.0.0 (every address, in a container)".into(),
        ),
    }
}

/// What `serve --web` runs: the standalone build in `dir`, with `node`, on `host`:`port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebOptions {
    pub dir: PathBuf,
    pub host: Ipv4Addr,
    pub port: u16,
    pub node: PathBuf,
    /// Open the sign-in link in the browser (`--open`).
    pub open: bool,
}

impl Args {
    pub fn docker_image(&self) -> Option<String> {
        let image = self.tex_image.trim();
        (!image.is_empty() && !image.eq_ignore_ascii_case("none")).then(|| image.to_owned())
    }

    pub fn resolve_data_dir(&self) -> anyhow::Result<PathBuf> {
        match &self.data_dir {
            Some(dir) => Ok(dir.clone()),
            None => dirs::data_local_dir()
                .map(|base| base.join("open-prism"))
                .context("cannot determine a data directory; pass --data-dir"),
        }
    }

    /// The web app to run next to the API: only with `serve --web <dir>`.
    pub fn web(&self) -> Option<WebOptions> {
        match &self.command {
            Some(Command::Serve(ServeArgs {
                web: Some(dir),
                web_port,
                web_host,
                node,
                open,
            })) => Some(WebOptions {
                dir: dir.clone(),
                host: *web_host,
                port: *web_port,
                node: node.clone(),
                open: *open,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("open-prism-api").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn serve_runs_the_web_app_on_the_given_ports() {
        let args = parse(&[
            "serve",
            "--web",
            "web/.next/standalone",
            "--port",
            "8799",
            "--web-port",
            "3799",
        ]);
        assert_eq!(args.port, 8799);
        assert_eq!(
            args.web(),
            Some(WebOptions {
                dir: PathBuf::from("web/.next/standalone"),
                host: Ipv4Addr::LOCALHOST,
                port: 3799,
                node: PathBuf::from("node"),
                open: false,
            })
        );
        let args = parse(&["serve", "--web", "d", "--node", "/opt/node/bin/node"]);
        assert_eq!(
            args.web().unwrap().node,
            PathBuf::from("/opt/node/bin/node")
        );
        assert!(
            parse(&["serve", "--web", "d", "--open"])
                .web()
                .unwrap()
                .open
        );
    }

    #[test]
    fn the_web_app_listens_on_loopback_or_every_address() {
        let host = |extra: &[&str]| {
            let mut args = vec!["serve", "--web", "d"];
            args.extend(extra);
            parse(&args).web().unwrap().host
        };
        if std::env::var_os("OPENPRISM_WEB_HOST").is_none() {
            assert_eq!(host(&[]), Ipv4Addr::LOCALHOST);
        }
        assert_eq!(host(&["--web-host", "0.0.0.0"]), Ipv4Addr::UNSPECIFIED);
        assert_eq!(host(&["--web-host", "127.0.0.1"]), Ipv4Addr::LOCALHOST);
        for other in ["192.168.1.5", "::", "localhost", "127.0.0.2"] {
            let err = Args::try_parse_from([
                "open-prism-api",
                "serve",
                "--web",
                "d",
                "--web-host",
                other,
            ])
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("127.0.0.1") && err.contains("0.0.0.0"),
                "{other}: {err}"
            );
        }
    }

    #[test]
    fn global_options_go_before_or_after_serve() {
        let args = parse(&["--port", "8799", "serve"]);
        assert_eq!(args.port, 8799);
        assert!(args.web().is_none());
        let args = parse(&["serve", "--tex-image", "none"]);
        assert_eq!(args.docker_image(), None);
    }

    #[test]
    fn without_web_only_the_api_runs() {
        let args = parse(&[]);
        assert!(args.command.is_none());
        if std::env::var_os("OPENPRISM_PORT").is_none() {
            assert_eq!(args.port, 8787);
        }
        assert!(args.web().is_none());
        assert!(parse(&["serve"]).web().is_none());
        assert!(matches!(
            parse(&["print-token"]).command,
            Some(Command::PrintToken)
        ));
    }

    #[test]
    fn web_options_belong_to_serve() {
        let parsed = Args::try_parse_from(["open-prism-api", "print-token", "--web", "d"]);
        assert!(parsed.is_err());
    }
}
