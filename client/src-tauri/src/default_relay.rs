//! The hosted default Farder relay, if one is configured. Filled in AFTER the
//! default relay is deployed -- see docs/deploy/relay.md. `None` means no default
//! relay is configured (users connect via custom/self-hosted relay links only).
//!
//! Not consumed by any code yet: this is the post-deploy config target and the
//! stable anchor that future phases (in-app relayed-server creation, shorter
//! invite links) will read.

use std::net::SocketAddr;

pub struct DefaultRelay {
    /// The relay's address, e.g. "203.0.113.42:4433". A literal IP:port — the
    /// parser below does not resolve hostnames, so `relay.farder.xyz:4433` here
    /// would silently leave the build with no default relay.
    pub addr: &'static str,
    /// SHA-256 of the relay's certificate DER, hex (64 chars).
    pub cert_fp_hex: &'static str,
}

/// The configured default relay, or `None` until one is deployed and filled in.
///
/// Deployed 2026-09-19 on a Vultr VPS (Ubuntu 24.04, Docker — see
/// `docs/deploy/relay.md`). This REPLACES the 2026-06-10 relay, whose host
/// lapsed; that machine's identity was never backed up, which is why this entry
/// changed at all and why every client built before today cannot talk to this
/// relay. The identity of THIS one is backed up off the box, so a future move
/// can restore it and leave this constant alone.
///
/// A literal `IP:port`. `parse_relay_config` below parses it as a `SocketAddr`,
/// so a hostname here (`relay.farder.xyz:4433`) silently yields NO default
/// relay. `relay.farder.xyz` exists in DNS and points at this same address, for
/// humans and for the day the client learns to resolve names.
pub const DEFAULT_RELAY: Option<DefaultRelay> = Some(DefaultRelay {
    addr: "45.61.136.29:4433",
    cert_fp_hex: "cbe837a3b1858c7f2246d290761101aa3e53157ca52c3cfb6128a010bd1d9bf0",
});

/// The TCP port the relay serves incoming-webhook HTTP on -- farder-relay's
/// `--webhook-bind` default (`0.0.0.0:8080`). Change both together.
pub const DEFAULT_RELAY_WEBHOOK_PORT: u16 = 8080;

/// Parse a relay address + hex fingerprint into a (SocketAddr, 32-byte fp).
/// Returns None if the address is unparseable or the fingerprint isn't 32 hex bytes.
fn parse_relay_config(addr: &str, fp_hex: &str) -> Option<(SocketAddr, Vec<u8>)> {
    let addr: SocketAddr = addr.parse().ok()?;
    let fp = hex::decode(fp_hex).ok()?;
    if fp.len() != 32 {
        return None;
    }
    Some((addr, fp))
}

/// The configured default relay as a parsed (SocketAddr, cert fingerprint), or
/// None if no default relay is configured (or it is malformed).
pub fn default_relay() -> Option<(SocketAddr, Vec<u8>)> {
    let r = DEFAULT_RELAY.as_ref()?;
    parse_relay_config(r.addr, r.cert_fp_hex)
}

/// The base URL an external service POSTs incoming webhooks to, e.g.
/// `http://203.0.113.42:8080` -- the relay's IP with the webhook port. `None`
/// when this build has no default relay (webhooks need the relay's inbound HTTP).
pub fn default_relay_webhook_base() -> Option<String> {
    let (addr, _) = default_relay()?;
    Some(format!(
        "http://{}",
        SocketAddr::new(addr.ip(), DEFAULT_RELAY_WEBHOOK_PORT)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_addr_and_fingerprint() {
        let got = parse_relay_config("203.0.113.42:4433", "7e3ed9b35aedcf3b42c30500720ca12cb1385ad0a74207b3f977167f1ab48459");
        let (addr, fp) = got.expect("should parse");
        assert_eq!(addr.port(), 4433);
        assert_eq!(fp.len(), 32);
    }

    #[test]
    fn rejects_bad_addr_or_fingerprint() {
        assert!(parse_relay_config("not-an-addr", "7e3ed9b3").is_none());
        assert!(parse_relay_config("203.0.113.42:4433", "zzzz").is_none());
        assert!(parse_relay_config("203.0.113.42:4433", "7e3e").is_none()); // not 32 bytes
    }

    #[test]
    fn default_relay_is_configured() {
        assert!(default_relay().is_some());
    }

    #[test]
    fn webhook_base_is_the_relay_ip_on_the_webhook_port() {
        let base = default_relay_webhook_base().expect("default relay configured");
        let (addr, _) = default_relay().expect("default relay configured");
        assert_eq!(base, format!("http://{}:{}", addr.ip(), DEFAULT_RELAY_WEBHOOK_PORT));
        assert!(base.ends_with(":8080"), "the webhook port, not the QUIC port");
    }
}
