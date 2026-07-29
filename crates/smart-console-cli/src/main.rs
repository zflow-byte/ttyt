#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!("smart-console starting");
    Ok(())
}
