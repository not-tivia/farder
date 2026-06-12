//! Invite-preview fetch proxy (relay fetch proxy, phase one). The relay asks a
//! target server "what's behind this invite code?" on a requester's behalf so
//! the requester's IP never touches the server host. Guardrails: per-IP rate
//! limit (router-level), 5s timeout, SSRF refusal of non-global addresses,
//! 16KB answer cap, 60s TTL cache.

use anyhow::Result;
use farder_protocol::codec;
use farder_protocol::messages::{PreviewOutcome, PreviewTarget};
use farder_protocol::server::{ClientFrame, RelayStreamRole, ServerFrame};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::debug;

pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(5);
const ANSWER_CAP: usize = 16 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_MAX_ENTRIES: usize = 1024;

// ---------------------------------------------------------------------------
// SSRF guard
// ---------------------------------------------------------------------------

/// Only globally-routable addresses may be dialed on a requester's behalf —
/// the relay must not be usable to probe its own host or private networks.
pub fn is_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64/10 CGNAT
                || v4.octets() == [192, 0, 0, 0]
                )
        }
        IpAddr::V6(v6) => {
            // Classic bypass: ::ffff:127.0.0.1 etc. — judge the embedded v4
            // address by v4 rules. Also refuse 6to4/NAT64 prefixes that embed
            // v4 addresses we can't easily judge.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_global_ip(std::net::IpAddr::V4(v4));
            }
            let seg = v6.segments();
            if seg[0] == 0x2002 || (seg[0] == 0x64 && seg[1] == 0xff9b) {
                return false;
            }
            !(v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
                )
        }
    }
}

// ---------------------------------------------------------------------------
// TTL cache
// ---------------------------------------------------------------------------

pub struct PreviewCache {
    entries: Mutex<HashMap<(String, String), (Instant, PreviewOutcome)>>,
}

impl PreviewCache {
    pub fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, key: &(String, String), now: Instant) -> Option<PreviewOutcome> {
        let map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key).and_then(|(at, v)| {
            (now.duration_since(*at) < CACHE_TTL).then(|| v.clone())
        })
    }

    pub fn put(&self, key: (String, String), value: PreviewOutcome, now: Instant) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= CACHE_MAX_ENTRIES {
            // Cheap pressure valve: drop expired entries; if still full, start over.
            map.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL);
            if map.len() >= CACHE_MAX_ENTRIES {
                map.clear();
            }
        }
        map.insert(key, (now, value));
    }
}

pub fn cache_key(target: &PreviewTarget, code: &str) -> (String, String) {
    let t = match target {
        PreviewTarget::Registered { server_id } => format!("r:{}", hex::encode(server_id)),
        PreviewTarget::Direct { addr } => format!("d:{}", addr),
    };
    (t, code.to_string())
}

// ---------------------------------------------------------------------------
// Outbound QUIC endpoint (direct-server dials)
// ---------------------------------------------------------------------------

/// Permissive client endpoint for dialing DIRECT farder servers — they use
/// self-signed certs and the Farder client itself accepts them the same way,
/// so this matches the ecosystem's existing trust model for direct connects.
///
/// NOTE: this binds `0.0.0.0:0` (v4-only), so IPv6 direct targets will fail
/// the QUIC connect and collapse to `Unavailable` by construction today.
/// `is_global_ip` must remain v6-correct anyway so the SSRF guard is sound
/// if this is ever upgraded to a dual-stack (`[::]`) socket.
pub fn outbound_endpoint() -> Result<Endpoint> {
    #[derive(Debug)]
    struct SkipVerify;
    impl rustls::client::danger::ServerCertVerifier for SkipVerify {
        fn verify_server_cert(&self, _e: &rustls::pki_types::CertificateDer<'_>, _i: &[rustls::pki_types::CertificateDer<'_>], _n: &rustls::pki_types::ServerName<'_>, _o: &[u8], _t: rustls::pki_types::UnixTime) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> { Ok(rustls::client::danger::ServerCertVerified::assertion()) }
        fn verify_tls12_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
        fn verify_tls13_signature(&self, _m: &[u8], _c: &rustls::pki_types::CertificateDer<'_>, _d: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> { Ok(rustls::client::danger::HandshakeSignatureValid::assertion()) }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> { rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes() }
    }
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(SkipVerify))
        .with_no_client_auth();
    let cfg = quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    let mut ep = Endpoint::client("0.0.0.0:0".parse()?)?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

// ---------------------------------------------------------------------------
// The fetch itself
// ---------------------------------------------------------------------------

async fn read_capped(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(len <= ANSWER_CAP, "preview answer too large: {} bytes", len);
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_framed(send: &mut SendStream, data: &[u8]) -> Result<()> {
    send.write_all(&(data.len() as u32).to_be_bytes()).await?;
    send.write_all(data).await?;
    Ok(())
}

/// Speak the preview exchange on an established (send, recv) pair the server
/// treats as a primary/control stream: Challenge → GetInvitePreview → answer.
async fn ask_server(send: &mut SendStream, recv: &mut RecvStream, code: &str) -> Result<PreviewOutcome> {
    let first: ServerFrame = match codec::decode(&read_capped(recv).await?) {
        Ok(f) => f,
        Err(_) => return Ok(PreviewOutcome::Unavailable),
    };
    if !matches!(first, ServerFrame::Challenge { .. }) {
        return Ok(PreviewOutcome::Unavailable);
    }
    write_framed(send, &codec::encode(&ClientFrame::GetInvitePreview { code: code.to_string() })?).await?;
    match codec::decode::<ServerFrame>(&read_capped(recv).await?) {
        Ok(ServerFrame::InvitePreview { server_name, member_count, online_count }) => {
            Ok(PreviewOutcome::Preview { server_name, member_count, online_count })
        }
        Ok(ServerFrame::InvitePreviewError { .. }) => Ok(PreviewOutcome::Invalid),
        _ => Ok(PreviewOutcome::Unavailable),
    }
}

/// Direct-mode preview exchange on an established connection: the SERVER opens
/// the primary stream (mirror of the real client's accept_bi), then we speak
/// Challenge → GetInvitePreview → answer on it.
pub async fn preview_over_direct_conn(conn: &Connection, code: &str) -> Result<PreviewOutcome> {
    let (mut s, mut r) = conn.accept_bi().await?;
    ask_server(&mut s, &mut r, code).await
}

/// Resolve and fetch a preview for `target`. Errors collapse to Unavailable at
/// the call site; this returns Result for `?` ergonomics on transport ops.
pub async fn fetch_preview(
    target: &PreviewTarget,
    code: &str,
    registered: Option<Connection>,
    out_endpoint: &Endpoint,
) -> PreviewOutcome {
    let attempt: Result<PreviewOutcome> = async {
        match target {
            PreviewTarget::Registered { .. } => {
                let Some(server_conn) = registered else {
                    return Ok(PreviewOutcome::Unavailable);
                };
                let (mut s, mut r) = server_conn.open_bi().await?;
                // Reserved handle 0 marks the stream as relay-originated.
                s.write_all(&0u32.to_be_bytes()).await?;
                write_framed(&mut s, &codec::encode(&RelayStreamRole::Primary)?).await?;
                ask_server(&mut s, &mut r, code).await
            }
            PreviewTarget::Direct { addr } => {
                let sock: SocketAddr = match addr.parse() {
                    Ok(s) => s,
                    Err(_) => return Ok(PreviewOutcome::Unavailable),
                };
                if !is_global_ip(sock.ip()) {
                    debug!("preview refused: non-global address {}", sock);
                    return Ok(PreviewOutcome::Unavailable);
                }
                let conn = out_endpoint.connect(sock, "farder-server")?.await?;
                let outcome = preview_over_direct_conn(&conn, code).await;
                conn.close(0u32.into(), b"preview done");
                outcome
            }
        }
    }
    .await;
    attempt.unwrap_or(PreviewOutcome::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_guard_refuses_non_global() {
        for bad in [
            "127.0.0.1", "10.0.0.1", "172.16.5.5", "192.168.1.1", "169.254.0.7",
            "0.0.0.0", "100.64.0.1", "::1", "fe80::1", "fc00::1", "::",
            // v4-mapped IPv6 (classic SSRF bypass)
            "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:192.168.1.1",
            // 6to4 (2002::/16) and NAT64 (64:ff9b::/96) tunnels
            "2002:7f00:1::", "64:ff9b::7f00:1",
        ] {
            assert!(!is_global_ip(bad.parse().unwrap()), "{bad} must be refused");
        }
        for good in ["203.0.113.7", "45.77.70.199", "2607:f8b0::1", "::ffff:8.8.8.8"] {
            assert!(is_global_ip(good.parse().unwrap()), "{good} must be allowed");
        }
    }

    #[test]
    fn cache_ttl_and_pressure() {
        let cache = PreviewCache::new();
        let t0 = Instant::now();
        let key = ("r:aa".to_string(), "code".to_string());
        assert!(cache.get(&key, t0).is_none());
        cache.put(key.clone(), PreviewOutcome::Invalid, t0);
        assert_eq!(cache.get(&key, t0), Some(PreviewOutcome::Invalid));
        // Within TTL.
        assert!(cache.get(&key, t0 + Duration::from_secs(59)).is_some());
        // Expired.
        assert!(cache.get(&key, t0 + Duration::from_secs(61)).is_none());
    }

    #[test]
    fn cache_key_distinguishes_targets_and_codes() {
        let a = cache_key(&PreviewTarget::Registered { server_id: vec![1] }, "x");
        let b = cache_key(&PreviewTarget::Registered { server_id: vec![2] }, "x");
        let c = cache_key(&PreviewTarget::Direct { addr: "1.2.3.4:1".into() }, "x");
        let d = cache_key(&PreviewTarget::Registered { server_id: vec![1] }, "y");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }
}
