use anyhow::{Context, Result};
use clap::Parser;
use farder_server::{auth, connection, db, members, permissions, retention, state::ServerState, templates};
use quinn::Endpoint;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "farder-server", about = "Farder community server")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:4435")]
    bind: SocketAddr,
    #[arg(long, default_value = "My Farder Server")]
    name: String,
    #[arg(long, default_value = "farder-server.db")]
    db: String,
    #[arg(long, default_value = "blank")]
    template: String,
    #[arg(long, default_value = "3600")]
    retention_interval: u64,
    #[arg(long, default_value = "./files")]
    storage_dir: String,
    #[arg(long, default_value = "52428800")]
    max_file_size: u64,

    /// If set, run relay-only: register with this relay instead of binding a
    /// public listener (hides the server's IP).
    #[arg(long)]
    relay: Option<SocketAddr>,

    /// Directory for the server's stable relay identity (server_id).
    #[arg(long, default_value = "./server-data")]
    data_dir: std::path::PathBuf,

    /// Keep automatic backups in this folder (a backup is a plain folder you can copy to
    /// another machine). One is made at start and then every --backup-interval seconds.
    #[arg(long)]
    backup_dir: Option<std::path::PathBuf>,
    /// Seconds between automatic backups (default 6 hours).
    #[arg(long, default_value = "21600")]
    backup_interval: u64,
    /// How many automatic backups to keep; older ones are deleted (default 14, minimum 1).
    #[arg(long, default_value = "14")]
    backup_keep: usize,
    /// Make ONE backup into this folder and exit. Safe to run while the server is running.
    #[arg(long)]
    backup_now: Option<std::path::PathBuf>,
    /// Restore the backup in this folder (verified first) over --db, --storage-dir and
    /// --data-dir, then exit. The server must not be running.
    #[arg(long)]
    restore_from: Option<std::path::PathBuf>,
    /// With --restore-from: replace an existing server. The old data is moved aside, not deleted.
    #[arg(long)]
    force: bool,
    /// Host-side owner reset, then exit: make this member (vk_ + 64 hex) the owner. For a
    /// server whose owner lost the account and has no recovery key. The server must be stopped.
    /// Takes a backup first, moves the old signed history aside (not deleted). Friends rejoin
    /// with a fresh invite; channels and plaintext messages stay. See docs/hosting-on-ubuntu.md.
    #[arg(long)]
    reset_owner: Option<String>,

    /// How many seconds a recovery-key takeover must wait before it can finish (default 72
    /// hours). The waiting period is what lets the real owner notice and react, so values
    /// under 24 hours are for testing only and are warned about at start.
    #[arg(long, default_value_t = farder_server::state::DEFAULT_RECOVERY_WAIT_SECS)]
    recovery_wait_secs: u64,

    /// How many destructive changes (deleting channels/categories/roles, kicking, banning, removing
    /// bots or webhooks) one account may make in 10 minutes before the server says "slow down" (default 5,
    /// 0 turns it off). Protects the server if an account is stolen. See crates/farder-server/src/brake.rs.
    #[arg(long, default_value_t = farder_server::brake::DEFAULT_BUDGET)]
    destructive_budget: usize,
}

fn make_server_endpoint(bind_addr: SocketAddr) -> Result<Endpoint> {
    let certified = rcgen::generate_simple_self_signed(vec!["farder-server".to_string()])?;
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
        .map_err(|e| anyhow::anyhow!("key error: {}", e))?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    // Enable QUIC datagrams (used by voice calls in v1).
    let mut transport = quinn::TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(1 << 20));
    transport.datagram_send_buffer_size(1 << 20);
    server_config.transport_config(Arc::new(transport));
    Ok(Endpoint::server(server_config, bind_addr)?)
}

async fn init_server(args: &Args) -> Result<(ServerState, bool)> {
    let conn = db::open_file(&args.db)?;
    let member_count: i64 = conn.query_row("SELECT count(*) FROM members", [], |row| row.get(0))?;
    let first_run = member_count == 0;

    if first_run {
        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true, false)?;
        let builtin = templates::list_builtin_templates();
        let template = builtin.iter()
            .find(|t| t.template.name.to_lowercase().replace(' ', "-") == args.template)
            .or_else(|| builtin.iter().find(|t| t.template.name == "Blank"));
        if let Some(t) = template {
            templates::apply_template(&conn, t)?;
            info!("Applied template: {}", t.template.name);
        }
    }

    let mut state = ServerState::new(conn, args.name.clone(), args.storage_dir.clone(), args.max_file_size);
    state.recovery_wait_secs = args.recovery_wait_secs;
    state.brake = farder_server::brake::Brake::new(args.destructive_budget, farder_server::brake::DEFAULT_WINDOW_SECS);
    if args.recovery_wait_secs < 24 * 3600 {
        tracing::warn!(
            "the recovery takeover waiting period is only {} seconds; under 24 hours it gives the real owner little time to notice (fine for testing)",
            args.recovery_wait_secs
        );
    }
    if !first_run {
        // Detect owner: first member by joined_at
        let owner_key: Option<Vec<u8>> = {
            let db = state.db.lock().unwrap();
            db.query_row(
                "SELECT public_key FROM members WHERE banned = 0 AND revoked = 0 ORDER BY joined_at ASC LIMIT 1",
                [], |row| row.get(0),
            ).ok()
        };
        if let Some(key_bytes) = owner_key {
            if let Ok(arr) = <[u8; 32]>::try_from(key_bytes.as_slice()) {
                let pk = farder_crypto::identity::PublicKey::from_bytes(arr);
                *state.owner.write().await = Some(pk);
            }
        }
    }

    Ok((state, first_run))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args = Args::parse();

    // One-shot jobs that do not start the server.
    if let Some(from) = &args.restore_from {
        let m = farder_server::backup::restore_backup(
            from,
            std::path::Path::new(&args.db),
            std::path::Path::new(&args.storage_dir),
            &args.data_dir,
            args.force,
        )?;
        info!("Restored the backup made at unix time {} ({} files). Start the server normally now.", m.created_at, m.files_count);
        return Ok(());
    }
    if let Some(key) = &args.reset_owner {
        let new_owner = farder_server::reset_owner::parse_member_key(key)?;
        let mut conn = db::open_file(&args.db)?;
        let pre = args.backup_dir.clone().unwrap_or_else(|| args.data_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from(".")).join("farder-pre-reset-backups"));
        let made = farder_server::backup::create_backup(&conn, std::path::Path::new(&args.storage_dir), &args.data_dir, &pre)?;
        info!("Backup made first: {}", made.display());
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        let r = farder_server::reset_owner::reset_owner(&mut conn, &new_owner, args.force, now)?;
        info!("Owner reset. Old server history id {} moved aside ({} signed records kept in events_before_reset_{}). New history id {}.",
              r.old_server_id, r.events_moved_aside, now, r.new_server_id);
        info!("Start the server normally. The new owner may need to reconnect; other friends rejoin with a fresh invite.");
        return Ok(());
    }
    if let Some(out) = &args.backup_now {
        let conn = db::open_file(&args.db)?;
        let made = farder_server::backup::create_backup(&conn, std::path::Path::new(&args.storage_dir), &args.data_dir, out)?;
        let removed = farder_server::backup::prune(out, args.backup_keep)?;
        info!("Backup written to {} ({} older backup(s) removed)", made.display(), removed);
        println!("{}", made.display());
        return Ok(());
    }

    let (server_state, first_run) = init_server(&args).await?;

    // Mesh Rung 1: if a genesis already exists in the DB, rebuild the in-memory
    // LogState so the server is ready to validate/append events without waiting
    // for an owner-establish trigger.
    {
        let conn = server_state.db.lock().unwrap();
        if let Some(g) = farder_server::event_ingest::load_genesis(&conn)? {
            let ls = farder_server::event_ingest::build_log_state(&conn)?;
            *server_state.genesis.lock().unwrap() = Some(g);
            *server_state.log_state.lock().unwrap() = ls;
            // Re-derive the channel-class mirror from the log FIRST. The column is
            // only a mirror of an accepted `ChannelCreated`; this re-asserts it,
            // and refuses (rather than resolves) any disagreement that would WIDEN
            // a channel the DB currently treats as sealed. It runs before
            // `reconcile_messages` because the message choke point resolves class
            // from that very column: a drifted mirror would make the sealed derive
            // door refuse the rows it is there to rebuild.
            // NOT `unwrap_or(0)`. This pass is the thing that repairs a mirror
            // saying `plaintext` for a channel the log declares `e2ee`, and a
            // drifted mirror is exactly the fail-OPEN state: every server-authored
            // writer (SendMessage, webhooks, slash commands, the sweeper) resolves
            // the class from that column and would be waved into a sealed channel.
            // Swallowing the error boots a server whose repair silently did not
            // run. Refusing to start is the loud answer, and the only one
            // consistent with what `channel_class.rs` promises.
            let reclassed = farder_server::channel_class::reconcile_channel_classes(&conn)
                .context("channel-class mirror repair failed; refusing to start")?;
            if reclassed > 0 {
                tracing::info!(count = reclassed, "repaired drifted channel class mirrors from the event log");
            }
            // The `LogState` is handed in so deletions survive the restart: this
            // pass re-derives every log message lacking a row, and without the
            // fold's tombstones it would re-derive the deleted ones too (spec F2).
            // Also not swallowed: this pass carries the F2 guarantee that a
            // content-blind delete survives a restart. A silent failure here
            // resurrects deleted messages on the next boot with nothing in the
            // log to say it happened.
            let repaired = {
                let guard = server_state.log_state.lock().unwrap();
                farder_server::event_ingest::reconcile_messages(&conn, guard.as_ref())
                    .context("event-sourced message reconciliation failed; refusing to start")?
            };
            if repaired > 0 {
                tracing::info!("reconciled {} event-sourced messages missing from the view", repaired);
            }
            let repaired_att = farder_server::event_ingest::reconcile_attachments(&conn)
                .context("attachment reconciliation failed; refusing to start")?;
            if repaired_att > 0 {
                tracing::info!(count = repaired_att, "reconciled missing attachment rows from the event log");
            }
            let swept = farder_server::event_ingest::sweep_redacted_bytes(&conn, &server_state.storage_dir).unwrap_or(0);
            if swept > 0 { tracing::info!(count = swept, "swept bytes of already-redacted attachments"); }
            drop(conn);
        }
    }

    std::fs::create_dir_all(&args.storage_dir)?;
    let state = Arc::new(server_state);

    if first_run {
        let setup_token = auth::generate_setup_token();
        let setup_hex = hex::encode(setup_token);
        info!("=== FIRST RUN ===");
        info!("The first user to connect will automatically become the server owner.");
        info!("For headless/remote setup, use this token instead: {}", setup_hex);
        *state.setup_token.lock().unwrap() = Some(setup_token);
    }

    // Automatic backups: one now, then one every --backup-interval seconds. They use their
    // own database connection (SQLite lets readers and the writer work side by side), so a
    // backup never makes the server wait.
    if let Some(dir) = args.backup_dir.clone() {
        let (db_path, storage, data, keep) = (args.db.clone(), args.storage_dir.clone(), args.data_dir.clone(), args.backup_keep);
        let every = std::time::Duration::from_secs(args.backup_interval.max(60));
        info!("Automatic backups to {} every {} seconds, keeping {}", dir.display(), every.as_secs(), keep.max(1));
        tokio::spawn(async move {
            loop {
                let (d, s2, dt, k, p) = (dir.clone(), storage.clone(), data.clone(), keep, db_path.clone());
                let r = tokio::task::spawn_blocking(move || -> anyhow::Result<std::path::PathBuf> {
                    let conn = db::open_file(&p)?;
                    let made = farder_server::backup::create_backup(&conn, std::path::Path::new(&s2), &dt, &d)?;
                    farder_server::backup::prune(&d, k)?;
                    Ok(made)
                })
                .await;
                match r {
                    Ok(Ok(made)) => info!("Backup written to {}", made.display()),
                    Ok(Err(e)) => tracing::warn!("automatic backup failed: {e:#}"),
                    Err(e) => tracing::warn!("automatic backup task failed: {e}"),
                }
                tokio::time::sleep(every).await;
            }
        });
    }

    let _retention = retention::spawn_retention_task(Arc::clone(&state), args.retention_interval);
    let _bot_poller = farder_server::bots::spawn_bot_poll_task(Arc::clone(&state));
    let _widget_sweeper = farder_server::widgets::spawn_widget_sweeper(Arc::clone(&state));

    if let Some(relay_addr) = args.relay {
        let server_id = farder_server::relay::load_or_generate_server_id(&args.data_dir)?;
        // Record the relay routing id (hex) so webhook URLs use the value the relay
        // routes by (NOT the mesh genesis).
        *state.relay_server_id.lock().unwrap() = Some(hex::encode(server_id));
        info!("Relay-only mode: registering with relay {}", relay_addr);
        farder_server::relay::serve_via_relay(state, relay_addr, server_id).await?;
        return Ok(());
    }

    let endpoint = make_server_endpoint(args.bind)?;
    info!("Server listening on {}", args.bind);

    loop {
        let incoming = match endpoint.accept().await {
            Some(inc) => inc,
            None => break,
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    let remote = conn.remote_address();
                    info!("New connection from {}", remote);
                    if let Err(e) = connection::handle_connection(state, conn).await {
                        info!("Client {} disconnected: {}", remote, e);
                    }
                }
                Err(e) => tracing::warn!("Connection handshake failed: {}", e),
            }
        });
    }
    Ok(())
}
