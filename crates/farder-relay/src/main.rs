mod config;
mod datagram;
mod embed;
mod limits;
mod listener;
mod proxy;
mod router;
mod webhook;

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
        300,                                 // max new connections per IP per window. One IP legitimately carries a lot: a home/office NAT shares its IP across many users, and a host runs both its client AND its local server from the same IP. 30 was far too strict.
        std::time::Duration::from_secs(60),  // the rate window
    ));
    let connections = router::new_state();
    let preview = router::new_preview_context()?;
    let embed = router::new_embed_context()?;
    let webhook_state = connections.clone();
    let webhook_limiter = std::sync::Arc::new(crate::limits::ConnectionLimiter::new(usize::MAX, 60, std::time::Duration::from_secs(60)));
    tokio::spawn(async move {
        if let Err(e) = webhook::serve(config.webhook_bind, webhook_state, webhook_limiter).await {
            tracing::error!("webhook server exited: {}", e);
        }
    });
    router::serve(endpoint, connections, limiter, preview, embed).await?;
    Ok(())
}
