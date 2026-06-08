//! Abuse controls for the relay: a global concurrent-connection cap and a
//! per-IP new-connection rate limit, so the public default relay can sit on the
//! internet without one source flooding it. The clock is injected (`now`) so the
//! logic is deterministically testable.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Held for a connection's lifetime; decrements the active count on drop.
pub struct ConnectionGuard {
    active: Arc<AtomicUsize>,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct ConnectionLimiter {
    max_connections: usize,
    rate_per_window: usize,
    window: Duration,
    active: Arc<AtomicUsize>,
    per_ip: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl ConnectionLimiter {
    pub fn new(max_connections: usize, rate_per_window: usize, window: Duration) -> Self {
        Self {
            max_connections,
            rate_per_window,
            window,
            active: Arc::new(AtomicUsize::new(0)),
            per_ip: Mutex::new(HashMap::new()),
        }
    }

    /// Try to admit a new connection from `ip` at `now`. Returns a guard (held for
    /// the connection's lifetime) if admitted, or `None` if the global cap or the
    /// per-IP rate limit is hit. Checks the cap first so a cap-refused attempt does
    /// not count against the IP's rate.
    pub fn try_admit(&self, ip: IpAddr, now: Instant) -> Option<ConnectionGuard> {
        // Global concurrent cap.
        let prev = self.active.fetch_add(1, Ordering::AcqRel);
        if prev >= self.max_connections {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        // Per-IP sliding-window rate limit.
        let over_rate = {
            let mut map = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
            let window = self.window;
            // Opportunistic prune: drop IPs whose most recent hit is outside the window.
            map.retain(|_, hits| hits.back().map(|t| now.duration_since(*t) < window).unwrap_or(false));
            let hits = map.entry(ip).or_default();
            while hits.front().map(|t| now.duration_since(*t) >= window).unwrap_or(false) {
                hits.pop_front();
            }
            if hits.len() >= self.rate_per_window {
                true
            } else {
                hits.push_back(now);
                false
            }
        };
        if over_rate {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(ConnectionGuard { active: self.active.clone() })
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn enforces_global_cap_and_guard_frees_a_slot() {
        let lim = ConnectionLimiter::new(2, 1000, Duration::from_secs(60));
        let now = Instant::now();
        let g1 = lim.try_admit(ip(1), now);
        let g2 = lim.try_admit(ip(2), now);
        assert!(g1.is_some() && g2.is_some());
        assert_eq!(lim.active(), 2);
        assert!(lim.try_admit(ip(3), now).is_none(), "over cap is refused");
        assert_eq!(lim.active(), 2, "a refused attempt does not increment active");
        drop(g1);
        assert_eq!(lim.active(), 1);
        assert!(lim.try_admit(ip(3), now).is_some(), "freed slot admits again");
    }

    #[test]
    fn enforces_per_ip_rate_window() {
        let lim = ConnectionLimiter::new(1000, 2, Duration::from_secs(60));
        let t0 = Instant::now();
        let _a = lim.try_admit(ip(1), t0).expect("1st");
        let _b = lim.try_admit(ip(1), t0).expect("2nd");
        assert!(lim.try_admit(ip(1), t0).is_none(), "3rd within window refused");
        assert!(lim.try_admit(ip(2), t0).is_some(), "other IP unaffected");
        let later = t0 + Duration::from_secs(61);
        assert!(lim.try_admit(ip(1), later).is_some(), "admitted after window");
    }

    #[test]
    fn cap_refusal_does_not_count_against_ip_rate() {
        let lim = ConnectionLimiter::new(1, 5, Duration::from_secs(60));
        let now = Instant::now();
        let _g = lim.try_admit(ip(1), now).expect("1st admitted");
        assert!(lim.try_admit(ip(1), now).is_none());
        drop(_g);
        assert!(lim.try_admit(ip(1), now).is_some(), "rate budget not consumed by cap refusal");
    }
}
