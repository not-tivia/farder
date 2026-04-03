use anyhow::{Context, Result};
use clap::Parser;
use farder_crypto::identity::Keypair;
use farder_node::node::PersonalNode;
use farder_protocol::{codec, messages::Message};
use quinn::{Endpoint, RecvStream, SendStream};
use rustls::pki_types::ServerName;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "farder-demo", about = "Farder P2P encrypted chat demo")]
struct Args {
    /// Listen on this address (User A mode: e.g. 127.0.0.1:5555)
    #[arg(long)]
    listen: Option<SocketAddr>,

    /// Connect to this address (User B mode: e.g. 127.0.0.1:5555)
    #[arg(long)]
    connect: Option<SocketAddr>,
}

// ---------------------------------------------------------------------------
// TLS helpers
// ---------------------------------------------------------------------------

/// Danger: skips server certificate verification. Development only.
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dh_params: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dh_params: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build a QUIC server endpoint with a freshly generated self-signed cert.
fn make_server_endpoint(bind_addr: SocketAddr) -> Result<Endpoint> {
    let certified = rcgen::generate_simple_self_signed(vec!["farder-demo".to_string()])?;
    let cert_der =
        rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der =
        rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
            .map_err(|e| anyhow::anyhow!("key error: {}", e))?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok(endpoint)
}

/// Build a QUIC client endpoint that skips cert verification.
fn make_client_endpoint() -> Result<Endpoint> {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

// ---------------------------------------------------------------------------
// Length-prefixed framing (matches farder-relay's router framing)
// ---------------------------------------------------------------------------

async fn read_message(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        anyhow::bail!("message too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_message(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Chat loop shared by both sides after key exchange completes
// ---------------------------------------------------------------------------

async fn chat_loop(
    node: &mut PersonalNode,
    peer_pub: farder_crypto::identity::PublicKey,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    println!("[info] Key exchange complete. Start typing messages (Ctrl-C to quit):");

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    loop {
        tokio::select! {
            // Read a line from stdin and send it
            line_result = lines.next_line() => {
                match line_result? {
                    None => {
                        // EOF on stdin
                        println!("[info] stdin closed, exiting.");
                        break;
                    }
                    Some(line) => {
                        if line.is_empty() {
                            continue;
                        }
                        let msg = node.prepare_dm(&peer_pub, line.as_bytes())
                            .context("failed to encrypt message")?;
                        let encoded = codec::encode(&msg)?;
                        write_message(&mut send, &encoded).await
                            .context("failed to send message")?;
                    }
                }
            }

            // Receive an incoming message from the peer
            recv_result = read_message(&mut recv) => {
                match recv_result {
                    Err(e) => {
                        println!("[info] Connection closed: {}", e);
                        break;
                    }
                    Ok(buf) => {
                        let msg: Message = codec::decode(&buf)
                            .context("failed to decode incoming message")?;
                        match msg {
                            Message::EncryptedDm { sender, ciphertext, timestamp } => {
                                let plaintext = node.receive_dm(&sender, &ciphertext, timestamp)
                                    .context("failed to decrypt message")?;
                                let text = String::from_utf8_lossy(&plaintext);
                                println!("[peer] {}", text);
                            }
                            other => {
                                println!("[warn] unexpected message during chat: {:?}", other);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// User A: listen for an incoming connection, complete key exchange as responder
// ---------------------------------------------------------------------------

async fn run_listener(bind_addr: SocketAddr) -> Result<()> {
    let keypair = Keypair::generate();
    let mut node = PersonalNode::new_in_memory(keypair)?;
    let pub_key = node.public_key();

    println!("[info] Your public key: {}", pub_key);
    println!("[info] Listening on {} — waiting for peer to connect...", bind_addr);

    let endpoint = make_server_endpoint(bind_addr)?;

    // Accept exactly one connection for this demo
    let incoming = endpoint
        .accept()
        .await
        .context("endpoint closed before any connection")?;
    let conn = incoming.await.context("connection handshake failed")?;
    println!("[info] Peer connected from {}", conn.remote_address());

    let (mut send, mut recv) = conn.accept_bi().await?;

    // 1. Receive KeyExchange from the initiator
    let buf = read_message(&mut recv).await?;
    let msg: Message = codec::decode(&buf)?;
    let (peer_pub, peer_session_pk) = match msg {
        Message::KeyExchange { sender, session_public_key } => {
            println!("[info] Received key exchange from {}", sender);
            (sender, session_public_key)
        }
        other => anyhow::bail!("expected KeyExchange, got {:?}", other),
    };

    // 2. Complete key exchange on our side
    node.complete_key_exchange(&peer_pub, &peer_session_pk);

    // 3. Send back KeyExchangeResponse
    let response = Message::KeyExchangeResponse {
        responder: node.public_key(),
        session_public_key: node.session_public_key_bytes(),
    };
    let encoded = codec::encode(&response)?;
    write_message(&mut send, &encoded).await?;

    println!("[info] Sent key exchange response.");

    // 4. Enter the chat loop
    chat_loop(&mut node, peer_pub, send, recv).await
}

// ---------------------------------------------------------------------------
// User B: connect to User A, initiate key exchange
// ---------------------------------------------------------------------------

async fn run_connector(server_addr: SocketAddr) -> Result<()> {
    let keypair = Keypair::generate();
    let mut node = PersonalNode::new_in_memory(keypair)?;
    let pub_key = node.public_key();

    println!("[info] Your public key: {}", pub_key);
    println!("[info] Connecting to {}...", server_addr);

    let endpoint = make_client_endpoint()?;

    // The server name must match what the server cert was generated for.
    let conn = endpoint
        .connect(server_addr, "farder-demo")?
        .await
        .context("failed to connect to peer")?;
    println!("[info] Connected.");

    let (mut send, mut recv) = conn.open_bi().await?;

    // 1. Send KeyExchange
    let ke_msg = Message::KeyExchange {
        sender: node.public_key(),
        session_public_key: node.session_public_key_bytes(),
    };
    let encoded = codec::encode(&ke_msg)?;
    write_message(&mut send, &encoded).await?;
    println!("[info] Sent key exchange.");

    // 2. Receive KeyExchangeResponse
    let buf = read_message(&mut recv).await?;
    let msg: Message = codec::decode(&buf)?;
    let (peer_pub, peer_session_pk) = match msg {
        Message::KeyExchangeResponse { responder, session_public_key } => {
            println!("[info] Received key exchange response from {}", responder);
            (responder, session_public_key)
        }
        other => anyhow::bail!("expected KeyExchangeResponse, got {:?}", other),
    };

    // 3. Complete key exchange on our side
    node.complete_key_exchange(&peer_pub, &peer_session_pk);

    // 4. Enter the chat loop
    chat_loop(&mut node, peer_pub, send, recv).await
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let args = Args::parse();

    match (args.listen, args.connect) {
        (Some(addr), None) => run_listener(addr).await,
        (None, Some(addr)) => run_connector(addr).await,
        (Some(_), Some(_)) => {
            anyhow::bail!("specify either --listen or --connect, not both")
        }
        (None, None) => {
            anyhow::bail!(
                "specify either --listen <addr> (User A) or --connect <addr> (User B)\n\
                 Example:\n  \
                   Terminal A: cargo run -p farder-demo -- --listen 127.0.0.1:5555\n  \
                   Terminal B: cargo run -p farder-demo -- --connect 127.0.0.1:5555"
            )
        }
    }
}
