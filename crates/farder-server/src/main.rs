mod auth;
mod channels;
mod connection;
mod db;
mod events;
mod handlers;
mod invites;
mod members;
mod messages;
mod permissions;
mod retention;
mod state;
mod templates;

fn main() {
    println!("farder-server v{}", env!("CARGO_PKG_VERSION"));
}
