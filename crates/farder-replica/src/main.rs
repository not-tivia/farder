use anyhow::Result;
use clap::Parser;
use farder_replica::{invite_from_link, load_or_create_key, sync_files, pull_once, state_path, PullError, Session};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Keep a verified copy of a Farder server on this machine. First run: give --invite (a normal invite
/// code) so this machine can join; it prints its key, which the server's owner must then allow as a
/// replica (Server Settings, or the SetReplica request). After that it pulls a fresh verified copy
/// every --interval seconds into --out (latest.db, previous.db, latest.json).
#[derive(Parser)]
struct Args {
    /// The server to copy (host:port, direct QUIC). Not needed when --invite is a link with the address in it.
    #[arg(long)]
    server: Option<SocketAddr>,
    /// SHA-256 (hex) of the server's certificate, to be sure it is the right machine.
    #[arg(long)]
    cert_fp: Option<String>,
    /// Folder for this replica's key, state and copies.
    #[arg(long, default_value = "farder-replica")]
    dir: PathBuf,
    /// A normal invite, needed only the first time: the link from the app's invite dialog
    /// (https://farder.xyz/join/...), a farder://host:port/CODE link, or just the code.
    #[arg(long)]
    invite: Option<String>,
    /// Seconds between copies (default 15 minutes).
    #[arg(long, default_value_t = 900)]
    interval: u64,
    /// The name members see for this machine in the member list.
    #[arg(long, default_value = "Backup copy")]
    name: String,
    /// Pull once and exit. Exit code 3 means "waiting for the owner to allow this replica".
    #[arg(long)]
    once: bool,
    /// Where a read-only copy server for this folder can be reached (host:port). After each pull the host
    /// is told, so a host that was replaced and restarts can ask the copy for the newer hosting term.
    #[arg(long)]
    announce: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let mut a = Args::parse();
    let mut server = a.server;
    if let Some(link) = a.invite.clone() {
        let (addr, code) = invite_from_link(&link)?;
        if server.is_none() { server = addr; }
        a.invite = Some(code);
    }
    let Some(server) = server else { anyhow::bail!("give --server host:port (or an --invite link that holds the address)") };
    std::fs::create_dir_all(&a.dir)?;
    let (key, fresh) = load_or_create_key(&a.dir.join("replica.key"))?;
    println!("replica key: {}", key.public_key());
    if fresh { println!("(new key made in {}; keep that file private)", a.dir.display()); }
    let fp = a.cert_fp.as_deref().map(hex::decode).transpose()?;
    let st = state_path(&a.dir);
    let mut joined = std::fs::read_to_string(&st).map(|t| t.contains("\"joined\":true")).unwrap_or(false);
    let mut announce_warned = false;
    loop {
        let mut s = match Session::connect(server, fp.clone(), &key, if joined { None } else { a.invite.clone() }, None).await {
            Ok(s) => s,
            Err(e) => { eprintln!("could not connect: {e:#}"); if a.once { std::process::exit(2); } tokio::time::sleep(std::time::Duration::from_secs(30)).await; continue; }
        };
        if !joined {
            let Some(inv) = a.invite.clone() else { eprintln!("this machine has not joined the server yet: run once with --invite <code>"); std::process::exit(2) };
            s.join_history(&key, &inv).await?;
            std::fs::write(&st, "{\"joined\":true}")?;
            joined = true;
            println!("joined the server as a member");
        }
        if let Err(e) = s.set_name(&key, &a.name).await { eprintln!("{e:#}"); }
        match pull_once(&mut s, &a.dir).await {
            Ok(info) => {
                println!("copy saved: {} bytes, {} signed events, history {}", info.size, info.events, info.history_id);
                match sync_files(&mut s, &a.dir).await {
                    Ok(f) => println!("files: {} fetched, {} already here, {} removed, {} not available", f.fetched, f.already_had, f.removed, f.unavailable),
                    Err(e) => { eprintln!("files: sync stopped (the database copy is saved): {e:#}"); if a.once { std::process::exit(1); } }
                }
                if let Some(addr) = &a.announce { s.announce_copy(addr, &mut announce_warned).await; }
            }
            Err(PullError::NotAllowed(why)) => {
                println!("waiting: the owner has not allowed this key as a replica yet ({why}). Allow {}", key.public_key());
                if a.once { std::process::exit(3); }
            }
            Err(PullError::Promoted(term)) => {
                eprintln!("this copy was promoted to host (term {term}); not overwriting it. Stop this replica for that folder: the promoted server owns latest.db now.");
                std::process::exit(4);
            }
            Err(PullError::Other(e)) => { eprintln!("copy failed (the previous good copy is kept): {e:#}"); if a.once { std::process::exit(1); } }
        }
        if a.once { return Ok(()); }
        tokio::time::sleep(std::time::Duration::from_secs(a.interval)).await;
    }
}
