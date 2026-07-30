#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod cli;
mod commands;

use clap::Parser;
use cli::{Cli, Command};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Command::ListDevices => commands::list_devices(),
        Command::Connect { ports, baud } => commands::connect(ports, baud).await,
        Command::Replay { path, speed } => commands::replay(path, speed).await,
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
