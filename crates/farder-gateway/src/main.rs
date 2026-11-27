use anyhow::{bail, Result};
use clap::Parser;
use farder_gateway::{serve, GatewayConfig, Route};
use std::net::SocketAddr;

/// Browser gateway for guest links. Put a TLS-terminating web server (Caddy) in front of it so
/// browsers reach it as wss://.
#[derive(Parser)]
struct Args {
    /// Address to listen on for WebSocket connections (keep it on localhost behind Caddy).
    #[arg(long, default_value = "127.0.0.1:8480")]
    listen: SocketAddr,
    /// A server browsers may reach: name=host:port[,fp=<sha256 hex of its certificate>].
    /// A route without fp does NOT check the certificate: only for a server on this machine/LAN.
    #[arg(long = "route")]
    routes: Vec<String>,
    /// Simultaneous connections allowed per client address.
    #[arg(long, default_value_t = 8)]
    max_per_ip: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let args = Args::parse();
    let mut cfg = GatewayConfig { max_per_ip: args.max_per_ip, ..Default::default() };
    for r in &args.routes {
        let (name, rest) = r.split_once('=').ok_or_else(|| anyhow::anyhow!("route must look like name=host:port[,fp=hex]"))?;
        let mut parts = rest.split(',');
        let addr: SocketAddr = parts.next().unwrap().parse()?;
        let mut cert_fp = None;
        for p in parts {
            match p.strip_prefix("fp=") {
                Some(h) => cert_fp = Some(hex::decode(h)?),
                None => bail!("unknown route option {p}"),
            }
        }
        cfg.routes.insert(name.to_string(), Route { addr, cert_fp });
    }
    if cfg.routes.is_empty() { bail!("give at least one --route name=host:port"); }
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!("gateway listening on ws://{} for {} server(s)", args.listen, cfg.routes.len());
    serve(listener, cfg).await
}
