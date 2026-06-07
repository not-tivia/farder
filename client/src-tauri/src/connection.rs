use anyhow::{bail, Context, Result};
use farder_crypto::identity::Keypair;
use farder_protocol::{
    codec,
    server::{ClientFrame, ServerFrame},
};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::net::SocketAddr;

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Connection info for a relayed server, parsed from the escape-hatch link
/// `farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<invite_token>`.
#[derive(Clone, Debug)]
pub struct RelayTarget {
    pub relay_addr: SocketAddr,
    pub server_id: Vec<u8>,
    pub cert_fp: Vec<u8>,
    pub invite_token: String,
}

/// Parse a relay link, or `None` if `s` is not a well-formed relay link (e.g. a
/// direct `farder://addr/code` link or anything else).
pub fn parse_relay_target(s: &str) -> Option<RelayTarget> {
    let rest = s.strip_prefix("farder://relay/")?;
    let parts: Vec<&str> = rest.splitn(4, '/').collect();
    if parts.len() != 4 {
        return None;
    }
    let relay_addr: SocketAddr = parts[0].parse().ok()?;
    let server_id = hex::decode(parts[1]).ok()?;
    let cert_fp = hex::decode(parts[2]).ok()?;
    if server_id.is_empty() || cert_fp.is_empty() || parts[3].is_empty() {
        return None;
    }
    Some(RelayTarget {
        relay_addr,
        server_id,
        cert_fp,
        invite_token: parts[3].to_string(),
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_relay_link() {
        let t = parse_relay_target(
            "farder://relay/1.2.3.4:4433/aabb/ccdd/inv123",
        )
        .expect("valid");
        assert_eq!(t.relay_addr, "1.2.3.4:4433".parse().unwrap());
        assert_eq!(t.server_id, vec![0xaa, 0xbb]);
        assert_eq!(t.cert_fp, vec![0xcc, 0xdd]);
        assert_eq!(t.invite_token, "inv123");
    }

    #[test]
    fn rejects_non_relay_or_malformed() {
        assert!(parse_relay_target("farder://1.2.3.4:4435/inv").is_none()); // direct form
        assert!(parse_relay_target("farder://relay/1.2.3.4:4433/aabb").is_none()); // too few parts
        assert!(parse_relay_target("https://example.com").is_none());
        assert!(parse_relay_target("farder://relay/notanaddr/aa/bb/t").is_none()); // bad addr
    }
}
