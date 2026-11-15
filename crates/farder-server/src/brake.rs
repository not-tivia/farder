//! The bulk-damage brake: a limit on how many DESTRUCTIVE changes (deleting channels, categories or
//! roles, kicking or banning people, removing bots or webhooks) one account can make in a short time.
//!
//! Why: if an account is stolen, the thief's first move is to wipe the server. An owner cleaning up
//! rarely deletes more than a handful of things in ten minutes, so a small budget costs a real owner
//! almost nothing and stops a thief after a few. It applies to the owner too (a stolen owner key is the
//! case it exists for). It is kept in memory: a thief cannot restart the server to reset it, and a
//! restart by the host only gives a fresh budget to everyone.
//!
//! This is a brake, not the waiting-period-and-cancel design (which needs a pending-action system):
//! it limits the damage rate, and every block lands in the audit log for people who manage the server.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

pub const DEFAULT_BUDGET: usize = 5;
pub const DEFAULT_WINDOW_SECS: u64 = 600;

pub struct Brake {
    /// Destructive changes allowed per account per window. 0 turns the brake off.
    pub budget: usize,
    pub window_secs: u64,
    log: Mutex<HashMap<[u8; 32], VecDeque<u64>>>,
}

impl Brake {
    pub fn new(budget: usize, window_secs: u64) -> Self {
        Self { budget, window_secs, log: Mutex::new(HashMap::new()) }
    }

    /// `Ok(())` if `who` may make another destructive change now, else `Err(seconds until the oldest
    /// one in the window ages out)`.
    pub fn check(&self, who: &[u8; 32], now: u64) -> Result<(), u64> {
        if self.budget == 0 { return Ok(()); }
        let mut log = self.log.lock().unwrap();
        let q = log.entry(*who).or_default();
        while q.front().map_or(false, |t| now.saturating_sub(*t) >= self.window_secs) { q.pop_front(); }
        if q.len() >= self.budget {
            let oldest = *q.front().unwrap();
            return Err((oldest + self.window_secs).saturating_sub(now).max(1));
        }
        Ok(())
    }

    /// Count one destructive change that actually happened.
    pub fn record(&self, who: &[u8; 32], now: u64) {
        if self.budget == 0 { return; }
        self.log.lock().unwrap().entry(*who).or_default().push_back(now);
    }
}

impl Default for Brake {
    fn default() -> Self { Self::new(DEFAULT_BUDGET, DEFAULT_WINDOW_SECS) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_allows_the_budget_then_blocks_until_the_oldest_ages_out() {
        let b = Brake::new(3, 100);
        let who = [1u8; 32];
        for t in [10, 20, 30] { assert!(b.check(&who, t).is_ok()); b.record(&who, t); }
        assert_eq!(b.check(&who, 40), Err(70), "blocked; the oldest (t=10) frees up at t=110");
        assert!(b.check(&who, 110).is_ok(), "frees exactly when the oldest leaves the window");
    }

    #[test]
    fn accounts_have_their_own_budgets_and_zero_means_off() {
        let b = Brake::new(1, 100);
        b.record(&[1; 32], 5);
        assert!(b.check(&[1; 32], 6).is_err());
        assert!(b.check(&[2; 32], 6).is_ok());
        let off = Brake::new(0, 100);
        for t in 0..50 { off.record(&[1; 32], t); }
        assert!(off.check(&[1; 32], 51).is_ok());
    }
}
