use clap::Parser;
use std::net::SocketAddr;

#[derive(Parser, Debug)]
#[command(name = "farder-relay", about = "Farder privacy relay node")]
pub struct Config {
    #[arg(long, default_value = "0.0.0.0:4433")]
    pub bind: SocketAddr,
    #[arg(long, default_value = "1024")]
    pub max_connections: u32,
}
