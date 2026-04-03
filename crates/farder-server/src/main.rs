use anyhow::Result;
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
}

fn make_server_endpoint(bind_addr: SocketAddr) -> Result<Endpoint> {
    let certified = rcgen::generate_simple_self_signed(vec!["farder-server".to_string()])?;
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
        .map_err(|e| anyhow::anyhow!("key error: {}", e))?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    Ok(Endpoint::server(server_config, bind_addr)?)
}

fn init_server(args: &Args) -> Result<(ServerState, bool)> {
    let conn = db::open_file(&args.db)?;
    let member_count: i64 = conn.query_row("SELECT count(*) FROM members", [], |row| row.get(0))?;
    let first_run = member_count == 0;

    if first_run {
        members::create_role(&conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true)?;
        let builtin = templates::list_builtin_templates();
        let template = builtin.iter()
            .find(|t| t.template.name.to_lowercase().replace(' ', "-") == args.template)
            .or_else(|| builtin.iter().find(|t| t.template.name == "Blank"));
        if let Some(t) = template {
            templates::apply_template(&conn, t)?;
            info!("Applied template: {}", t.template.name);
        }
    }

    let state = ServerState::new(conn, args.name.clone());
    if !first_run {
        // Detect owner: first member by joined_at
        let db = state.db.lock().unwrap();
        let owner_key: Option<Vec<u8>> = db.query_row(
            "SELECT public_key FROM members WHERE banned = 0 AND revoked = 0 ORDER BY joined_at ASC LIMIT 1",
            [], |row| row.get(0),
        ).ok();
        drop(db);
        if let Some(key_bytes) = owner_key {
            if let Ok(arr) = <[u8; 32]>::try_from(key_bytes.as_slice()) {
                let pk = farder_crypto::identity::PublicKey::from_bytes(arr);
                *state.owner.blocking_write() = Some(pk);
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
    let (server_state, first_run) = init_server(&args)?;
    let state = Arc::new(server_state);

    if first_run {
        let setup_token = auth::generate_setup_token();
        let setup_hex = hex::encode(&setup_token);
        info!("=== FIRST RUN ===");
        info!("Setup token (give to the server owner): {}", setup_hex);
        info!("This token is single-use. The first user to connect with it becomes the Owner.");
        *state.setup_token.lock().unwrap() = Some(setup_token);
    }

    let _retention = retention::spawn_retention_task(Arc::clone(&state), args.retention_interval);

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
                    let (send, recv) = match conn.accept_bi().await {
                        Ok(s) => s,
                        Err(e) => { tracing::warn!("bi-stream error from {}: {}", remote, e); return; }
                    };
                    if let Err(e) = connection::handle_client(state, send, recv).await {
                        info!("Client {} disconnected: {}", remote, e);
                    }
                }
                Err(e) => tracing::warn!("Connection handshake failed: {}", e),
            }
        });
    }
    Ok(())
}
