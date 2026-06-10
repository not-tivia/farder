//! The hosted default Farder relay, if one is configured. Filled in AFTER the
//! default relay is deployed — see docs/deploy/relay.md. `None` means no default
//! relay is configured (users connect via custom/self-hosted relay links only).
//!
//! Not consumed by any code yet: this is the post-deploy config target and the
//! stable anchor that future phases (in-app relayed-server creation, shorter
//! invite links) will read.

#[allow(dead_code)]
pub struct DefaultRelay {
    /// The relay's address, e.g. "relay.farder.gg:4433".
    pub addr: &'static str,
    /// SHA-256 of the relay's certificate DER, hex (64 chars).
    pub cert_fp_hex: &'static str,
}

/// The configured default relay, or `None` until one is deployed and filled in.
/// Deployed 2026-06-10 on a Vultr VPS (Docker, see docs/deploy/relay.md).
#[allow(dead_code)]
pub const DEFAULT_RELAY: Option<DefaultRelay> = Some(DefaultRelay {
    addr: "45.77.70.199:4433",
    cert_fp_hex: "7e3ed9b35aedcf3b42c30500720ca12cb1385ad0a74207b3f977167f1ab48459",
});
