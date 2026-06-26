//! SSRF guard for server-side URL fetches (the `FetchUrl` request).
//!
//! The server must never be usable to probe its own host or private networks:
//! a member can post an arbitrary URL, and without this guard the server would
//! dial it directly (cloud-metadata `169.254.169.254`, `localhost:<admin>`,
//! LAN/router IPs, etc.).
//!
//! `is_global_ip` is a deliberate, line-for-line mirror of
//! `farder_relay::proxy::is_global_ip` (the reviewed relay guard). Keep the two
//! in sync; a future cleanup may hoist this into a shared crate. We can't depend
//! on `farder-relay` from `farder-server`, so it is duplicated here.

use std::net::IpAddr;

/// Only globally-routable addresses may be dialed on a requester's behalf.
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
                || v4.octets() == [192, 0, 0, 0])
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
                || (seg[0] & 0xffc0) == 0xfe80) // fe80::/10 link local
        }
    }
}

/// True only if the URL parses, its host resolves, AND every resolved IP is
/// globally routable. Fails CLOSED on any parse/resolve error or empty
/// resolution. Note: like the relay's guard, this resolves and then the caller
/// dials by hostname, so a determined DNS-rebinding attacker could still race
/// the resolution — a known residual tracked for the shared-guard cleanup
/// (pin-the-resolved-IP). It nonetheless blocks the common SSRF targets.
pub async fn resolves_to_global(raw: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(raw) else {
        return false;
    };
    let Some(host) = u.host_str() else {
        return false;
    };
    let port = u.port_or_known_default().unwrap_or(443);
    let Ok(addrs) = tokio::net::lookup_host((host, port)).await else {
        return false;
    };
    let mut any = false;
    for a in addrs {
        any = true;
        if !is_global_ip(a.ip()) {
            return false;
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn rejects_loopback_private_linklocal_metadata() {
        // The classic SSRF targets must all be refused.
        assert!(!is_global_ip(ip("127.0.0.1")));
        assert!(!is_global_ip(ip("10.0.0.5")));
        assert!(!is_global_ip(ip("192.168.1.1")));
        assert!(!is_global_ip(ip("172.16.0.1")));
        assert!(!is_global_ip(ip("169.254.169.254"))); // cloud metadata + link-local
        assert!(!is_global_ip(ip("100.64.0.1"))); // CGNAT
        assert!(!is_global_ip(ip("0.0.0.0"))); // unspecified
        assert!(!is_global_ip(ip("::1"))); // v6 loopback
        assert!(!is_global_ip(ip("fc00::1"))); // v6 ULA
        assert!(!is_global_ip(ip("fe80::1"))); // v6 link-local
        assert!(!is_global_ip(ip("::ffff:127.0.0.1"))); // v4-mapped loopback bypass
    }

    #[test]
    fn allows_global() {
        assert!(is_global_ip(ip("1.1.1.1")));
        assert!(is_global_ip(ip("8.8.8.8")));
        assert!(is_global_ip(ip("2606:4700:4700::1111"))); // Cloudflare v6
    }

    #[tokio::test]
    async fn resolves_to_global_refuses_local_hosts() {
        // No external network needed — these resolve locally.
        assert!(!resolves_to_global("http://127.0.0.1/").await);
        assert!(!resolves_to_global("http://localhost/").await);
        assert!(!resolves_to_global("http://169.254.169.254/latest/meta-data/").await);
        assert!(!resolves_to_global("not a url").await);
        // NOTE: scheme filtering (http/https only) is enforced upstream in
        // handle_fetch_url, not here — this guard only judges IP globalness, so
        // it intentionally does not reject e.g. ftp:// (kept out of tests to
        // avoid an external DNS lookup).
    }
}
