mod config;
mod limits;
mod listener;
mod router;

use anyhow::Result;
use clap::Parser;
use config::Config;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "farder_relay=info".into()),
        )
        .init();
    let config = Config::parse();
    info!("Starting Farder Relay v{}", env!("CARGO_PKG_VERSION"));
    let endpoint = listener::create_endpoint(config.bind, &config.data_dir)?;
    let limiter = std::sync::Arc::new(limits::ConnectionLimiter::new(
        config.max_connections as usize,
        30,                                  // max new connections per IP per window
        std::time::Duration::from_secs(60),  // the rate window
    ));
    let connections = router::new_connection_map();
    router::serve(endpoint, connections, limiter).await?;
    Ok(())
}
