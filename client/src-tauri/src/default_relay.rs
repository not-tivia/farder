//! The hosted default Farder relay, if one is configured. Filled in AFTER the
//! default relay is deployed -- see docs/deploy/relay.md. `None` means no default
//! relay is configured (users connect via custom/self-hosted relay links only).
//!
//! Not consumed by any code yet: this is the post-deploy config target and the
//! stable anchor that future phases (in-app relayed-server creation, shorter
//! invite links) will read.

use std::net::SocketAddr;

pub struct DefaultRelay {
    /// The relay's address, e.g. "relay.farder.gg:4433".
    pub addr: &'static str,
    /// SHA-256 of the relay's certificate DER, hex (64 chars).
    pub cert_fp_hex: &'static str,
}

/// The configured default relay, or `None` until one is deployed and filled in.
/// Deployed 2026-06-10 on a Vultr VPS (Docker, see docs/deploy/relay.md).
pub const DEFAULT_RELAY: Option<DefaultRelay> = Some(DefaultRelay {
    addr: "45.77.70.199:4433",
    cert_fp_hex: "7e3ed9b35aedcf3b42c30500720ca12cb1385ad0a74207b3f977167f1ab48459",
});

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_addr_and_fingerprint() {
        let got = parse_relay_config("45.77.70.199:4433", "7e3ed9b35aedcf3b42c30500720ca12cb1385ad0a74207b3f977167f1ab48459");
        let (addr, fp) = got.expect("should parse");
        assert_eq!(addr.port(), 4433);
        assert_eq!(fp.len(), 32);
    }

    #[test]
    fn rejects_bad_addr_or_fingerprint() {
        assert!(parse_relay_config("not-an-addr", "7e3ed9b3").is_none());
        assert!(parse_relay_config("45.77.70.199:4433", "zzzz").is_none());
        assert!(parse_relay_config("45.77.70.199:4433", "7e3e").is_none()); // not 32 bytes
    }

    #[test]
    fn default_relay_is_configured() {
        assert!(default_relay().is_some());
    }
}
