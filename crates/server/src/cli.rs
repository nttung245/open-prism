use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

/// Open Prism API server. Listens on 127.0.0.1 only; the Next.js app is the public entry point.
#[derive(Debug, Parser)]
#[command(name = "open-prism-api", version, about)]
pub struct Args {
    /// Directory holding projects, build output, history, the database and the access token.
    /// Defaults to the platform data dir, e.g. ~/.local/share/open-prism.
    #[arg(long, env = "OPENPRISM_DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,

    /// TCP port on 127.0.0.1.
    #[arg(long, env = "OPENPRISM_PORT", default_value_t = 8787)]
    pub port: u16,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the API server (default).
    Serve,
    /// Print the access token used to sign in (creates it if needed).
    PrintToken,
}

impl Args {
    pub fn resolve_data_dir(&self) -> anyhow::Result<PathBuf> {
        match &self.data_dir {
            Some(dir) => Ok(dir.clone()),
            None => dirs::data_local_dir()
                .map(|base| base.join("open-prism"))
                .context("cannot determine a data directory; pass --data-dir"),
        }
    }
}
