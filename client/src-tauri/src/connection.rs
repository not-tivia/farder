use anyhow::{bail, Context, Result};
use farder_crypto::identity::Keypair;
use farder_protocol::{
    codec,
    server::{ClientFrame, ServerFrame},
};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::net::SocketAddr;

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Read a length-prefixed frame (4-byte big-endian length header).
pub async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .context("failed to read frame length")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        bail!("frame too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .context("failed to read frame body")?;
    Ok(buf)
}

/// Write a length-prefixed frame (4-byte big-endian length header).
pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len)
        .await
        .context("failed to write frame length")?;
    send.write_all(data)
        .await
        .context("failed to write frame body")?;
    Ok(())
}

/// Encode and send a `ClientFrame` over the stream.
pub async fn send_client_frame(send: &mut SendStream, frame: &ClientFrame) -> Result<()> {
    let encoded = codec::encode(frame).context("failed to encode ClientFrame")?;
    write_frame(send, &encoded).await
}

/// Read and decode a `ServerFrame` from the stream.
pub async fn recv_server_frame(recv: &mut RecvStream) -> Result<ServerFrame> {
    let buf = read_frame(recv).await?;
    let frame: ServerFrame = codec::decode(&buf).context("failed to decode ServerFrame")?;
    Ok(frame)
}

/// Connect to a Farder server, perform the challenge-response authentication,
/// and return the active connection, send/recv streams, and session token.
///
/// Auth flow:
///   1. Client connects via QUIC.
///   2. Server opens the main bi-stream; client accepts it.
///   3. Server sends `Challenge { nonce }`.
///   4. Client signs the nonce and sends `Authenticate { ... }`.
///   5. Server responds with `Authenticated { session_token }` or `AuthError`.
pub async fn connect_and_authenticate(
    endpoint: Endpoint,
    address: SocketAddr,
    keypair: &Keypair,
    invite_code: Option<String>,
    setup_token: Option<String>,
) -> Result<(Connection, SendStream, RecvStream, Vec<u8>)> {
    // Step 1: establish QUIC connection
    let conn = endpoint
        .connect(address, "farder-server")
        .context("failed to initiate QUIC connection")?
        .await
        .context("QUIC handshake failed")?;

    // Step 2: server opens the main bi-directional stream
    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .context("failed to accept bi-stream from server")?;

    // Step 3: receive challenge nonce
    let nonce = match recv_server_frame(&mut recv).await? {
        ServerFrame::Challenge { nonce } => nonce,
        other => bail!("expected Challenge, got {:?}", other),
    };

    // Step 4: sign the nonce and authenticate
    let signed_challenge = keypair.sign(&nonce);
    let public_key = keypair.public_key();
    let auth_frame = ClientFrame::Authenticate {
        public_key,
        signed_challenge,
        invite_code,
        setup_token,
    };
    send_client_frame(&mut send, &auth_frame)
        .await
        .context("failed to send Authenticate frame")?;

    // Step 5: receive authentication result
    let session_token = match recv_server_frame(&mut recv).await? {
        ServerFrame::Authenticated { session_token } => session_token,
        ServerFrame::AuthError { reason } => bail!("authentication failed: {}", reason),
        other => bail!("unexpected frame after auth: {:?}", other),
    };

    Ok((conn, send, recv, session_token))
}
