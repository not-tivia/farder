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
#[allow(dead_code)]
pub const DEFAULT_RELAY: Option<DefaultRelay> = None;
