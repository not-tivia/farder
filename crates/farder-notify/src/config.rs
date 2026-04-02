use clap::Parser;
use std::net::SocketAddr;

#[derive(Parser, Debug)]
#[command(name = "farder-notify", about = "Farder notification relay")]
pub struct Config {
    #[arg(long, default_value = "0.0.0.0:4434")]
    pub bind: SocketAddr,
    #[arg(long, default_value = "farder-notify.db")]
    pub db_path: String,
}
