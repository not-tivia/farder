mod config;
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
    let connections = router::new_connection_map();
    while let Some(incoming) = endpoint.accept().await {
        let conn = incoming.await?;
        let connections = connections.clone();
        tokio::spawn(async move {
            if let Err(e) = router::handle_connection(conn, connections).await {
                tracing::warn!("Connection error: {}", e);
            }
        });
    }
    Ok(())
}
