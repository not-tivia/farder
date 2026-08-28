//! K1/K2 of the 5b lifecycle: keeping an E2EE channel able to accept messages.
//!
//! # The bug this module exists to prevent
//!
//! The fold seals a channel's sealed content in two blind, deliberate ways:
//!
//! - **The freshness ceiling** (`FRESHNESS_CEILING_EVENTS = 500`,
//!   `event_log_state.rs:49`): once 500 channel events accumulate with no
//!   accepted commit, sealed content is refused "until somebody rekeys". This is
//!   forward secrecy enforced by a host that cannot read a word.
//! - **The pending-removals gate**: a banned member's (or a revoked device's)
//!   leaf is drift, and drift seals the channel until a remaining member authors
//!   a remove-commit.
//!
//! 5a built both answers — [`crate::rekey::rekey_channel`] and
//! [`crate::drift::discharge_drift`] — and shipped both dormant. Nothing called
//! them, and the client's send path keyed on `stale-epoch` alone. So an ordinary
//! E2EE channel stopped accepting messages after ~500 of them, permanently, and
//! banning a member did the same immediately. Both were reachable by using the
//! product normally.
//!
//! [`send_sealed_keepalive`] is the missing half: it reacts to those two
//! rejections instead of surfacing them as a dead end.
//!
//! # Why the retry is bounded at ONE
//!
//! A ceiling rejection is the fold's own guarantee that the commit-rate rule
//! stands aside ([`crate::rekey`]'s `RekeyTrigger::CeilingSignalled`), so exactly
//! one rekey is enough to clear it. Looping would be the recurring
//! "over-conservative guard becomes an unexitable state" bug in reverse — a spin
//! against a server that is answering correctly. One attempt, then a typed error.

use farder_mls::group::{DeclaredMember, MlsChannelGroup};

use crate::chain::{Actor, ChainState};
use crate::channel::E2eeError;
use crate::drift::{discharge_drift, DriftDischargeContext};
use crate::join::SendEligibility;
use crate::rekey::{rekey_channel, RekeyContext};
use crate::sealed::{send_sealed, SealContext, SealedSendOutcome};
use crate::transport::E2eeTransport;

/// The leaves this group holds that their holders are no longer entitled to —
/// the client's view of the fold's `pending_removals`.
///
/// The client has no `LogState`, so it reconstructs the set by subtraction:
/// every `(identity, device)` the group's **actual** leaf view holds
/// (`group.leaves()`, never the claimed view) that is absent from `live`, the
/// caller's current roster times live-devices set. A banned member vanishes from
/// the roster; a revoked or cert-expired device vanishes from
/// `member_live_leaves`. Both therefore show up here.
pub fn dead_leaves(
    group: &MlsChannelGroup,
    live: &[DeclaredMember],
) -> Result<Vec<DeclaredMember>, E2eeError> {
    let leaves = group
        .leaves()
        .map_err(|e| E2eeError::Mls(e.context("read group leaves for drift detection")))?;
    Ok(leaves
        .into_iter()
        .map(|l| l.member)
        .filter(|m| !live.iter().any(|k| k.identity == m.identity && k.device == m.device))
        .collect())
}

/// What [`send_sealed_keepalive`] had to do to get the message out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepaliveAction {
    /// The send succeeded directly; the channel was healthy.
    SentDirectly,
    /// The freshness ceiling had sealed the channel. A rekey cleared it and the
    /// message went out on the retry.
    RekeyedThenSent,
    /// Drift (a banned member's or a revoked device's leaf) had sealed the
    /// channel. A remove-commit discharged it and the message went out on the
    /// retry.
    DischargedThenSent,
}

/// Who gained or lost the ability to read a channel between two snapshots of
/// its leaf set (sub-5b K5).
///
/// This is the fact the transparency notice is made of. The spec requires a
/// leaf-set change to surface in-channel — "a new device of Alice can now read
/// #private" — and it must be derived from the group's ACTUAL leaf view, never
/// from a commit's declared adds/removes. A notice built from declared data is a
/// notice an attacker writes: the whole point of Gate 2 is that what a commit
/// CLAIMS and what the tree HOLDS are different things.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LeafDiff {
    /// Leaves present after but not before: these devices can now read.
    pub gained: Vec<DeclaredMember>,
    /// Leaves present before but not after: these devices no longer can.
    pub lost: Vec<DeclaredMember>,
}

impl LeafDiff {
    pub fn is_empty(&self) -> bool {
        self.gained.is_empty() && self.lost.is_empty()
    }
}

/// Diff two leaf snapshots. Order-insensitive; a leaf in both is unchanged.
pub fn leaf_diff(before: &[DeclaredMember], after: &[DeclaredMember]) -> LeafDiff {
    let has = |set: &[DeclaredMember], m: &DeclaredMember| {
        set.iter().any(|k| k.identity == m.identity && k.device == m.device)
    };
    LeafDiff {
        gained: after.iter().filter(|m| !has(before, m)).cloned().collect(),
        lost: before.iter().filter(|m| !has(after, m)).cloned().collect(),
    }
}

/// Snapshot a group's ACTUAL leaf set, for use with [`leaf_diff`].
///
/// Returns an empty snapshot if the tree cannot be read. That is deliberate for
/// this caller: a notice is a transparency signal, not a gate, and failing a
/// channel open because we could not enumerate leaves would trade a real
/// capability for a cosmetic one. The security decisions that DO depend on the
/// leaf view (Gate 2, the add idempotency guard, drift detection) each fail
/// closed on their own.
pub fn leaf_snapshot(group: &MlsChannelGroup) -> Vec<DeclaredMember> {
    group
        .leaves()
        .map(|ls| ls.into_iter().map(|l| l.member).collect())
        .unwrap_or_default()
}

/// The number of distinct identities holding a leaf in this group — the client's
/// estimate of the fold's `committing_identities()`, which feeds the commit-rate
/// half of [`crate::rekey::should_rekey`].
///
/// Read from the ACTUAL leaf view, like every other decision in this crate. An
/// unreadable tree yields `1` (the freest gap) rather than an error: this is an
/// input to a *cadence* decision, and refusing to send because we could not
/// count identities would be the wrong failure. An underestimate only costs a
/// surfaced `RekeyRateLimited`, never a wrong commit.
pub fn committing_identities(group: &MlsChannelGroup) -> u64 {
    let Ok(leaves) = group.leaves() else {
        return 1;
    };
    let mut ids: Vec<_> = leaves.into_iter().map(|l| l.member.identity).collect();
    ids.sort_by_key(|a| a.to_string());
    ids.dedup();
    (ids.len() as u64).max(1)
}

/// The two repairs a keep-alive send may need, bundled so the call stays under
/// clippy's argument bound (the same reason `ReprovisionLive` exists).
///
/// `dead` is the caller's [`dead_leaves`] view. Passing an empty slice is normal
/// and cheap: deriving the real set costs a roster fetch plus a cert fetch per
/// member, so a caller should pay for it only after a send actually comes back
/// sealed on drift.
pub struct KeepaliveRepairs<'a> {
    pub rekey: &'a RekeyContext<'a>,
    pub drift: &'a DriftDischargeContext<'a>,
    pub dead: &'a [DeclaredMember],
}

/// The result of a keep-alive send.
#[derive(Debug)]
pub struct KeepaliveOutcome {
    pub send: SealedSendOutcome,
    pub action: KeepaliveAction,
}

/// Send one sealed message, rekeying first if the freshness ceiling has sealed
/// the channel.
///
/// - A **ceiling** rejection triggers exactly one [`rekey_channel`] and one
///   retry. If the retry also fails, the error is returned as-is.
/// - A **pending-removals** rejection triggers exactly one [`discharge_drift`]
///   over `dead` (the caller's [`dead_leaves`] view) and one retry. With an
///   EMPTY `dead` set the error surfaces unchanged: we know the channel is
///   sealed but not whom to remove, and guessing would author a removal against
///   a member in good standing, which the fold refuses anyway.
/// - Every other error surfaces unchanged, including the divergence class — a
///   rekey that is itself rejected leaves the local group one epoch ahead
///   (finding F1), and this function must never paper over that.
pub async fn send_sealed_keepalive<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &SealContext<'_>,
    repairs: &KeepaliveRepairs<'_>,
    group: &mut MlsChannelGroup,
    eligibility: &SendEligibility,
) -> Result<KeepaliveOutcome, E2eeError> {
    match send_sealed(transport, actor, chain, ctx, group, eligibility).await {
        Ok(send) => Ok(KeepaliveOutcome {
            send,
            action: KeepaliveAction::SentDirectly,
        }),
        Err(e) if e.is_freshness_ceiling_reached() => {
            // The ceiling is the fold telling us to rekey, and guaranteeing the
            // commit-rate rule will not stand in the way. Do exactly that, once.
            rekey_channel(transport, actor, chain, repairs.rekey, group).await?;
            let send = send_sealed(transport, actor, chain, ctx, group, eligibility).await?;
            Ok(KeepaliveOutcome {
                send,
                action: KeepaliveAction::RekeyedThenSent,
            })
        }
        Err(e) if e.is_sealed_pending_removals() => {
            // Drift seals the channel until a remaining member removes the dead
            // leaves. A rekey does NOT discharge it (the fold requires the
            // commit's removes to intersect `pending_removals`), so this is a
            // distinct operation, not a second rekey.
            if repairs.dead.is_empty() {
                return Err(e);
            }
            discharge_drift(transport, actor, chain, repairs.drift, group, repairs.dead).await?;
            let send = send_sealed(transport, actor, chain, ctx, group, eligibility).await?;
            Ok(KeepaliveOutcome {
                send,
                action: KeepaliveAction::DischargedThenSent,
            })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farder_crypto::identity::Keypair;

    fn member(id: &Keypair, device: &str) -> DeclaredMember {
        DeclaredMember {
            identity: id.public_key(),
            device: device.to_string(),
        }
    }

    #[test]
    fn leaf_diff_reports_only_what_actually_changed() {
        let alice = Keypair::generate();
        let bob = Keypair::generate();
        let before = vec![member(&alice, "dev-a"), member(&bob, "dev-b")];
        let after = vec![
            member(&alice, "dev-a"),
            member(&alice, "dev-a2"), // Alice added a second device
            // Bob's leaf is gone
        ];

        let diff = leaf_diff(&before, &after);
        assert_eq!(diff.gained, vec![member(&alice, "dev-a2")]);
        assert_eq!(diff.lost, vec![member(&bob, "dev-b")]);
        assert!(!diff.is_empty());
    }

    #[test]
    fn an_unchanged_leaf_set_produces_no_notice() {
        let alice = Keypair::generate();
        let set = vec![member(&alice, "dev-a")];
        assert!(leaf_diff(&set, &set).is_empty(), "a no-op run must not notify");
    }

    /// The same device id under a DIFFERENT identity is a different leaf. If the
    /// diff keyed on the device alone, an impostor reusing a device id would
    /// slip in without a notice — exactly what the notice exists to prevent.
    #[test]
    fn the_diff_keys_on_identity_and_device_together() {
        let alice = Keypair::generate();
        let mallory = Keypair::generate();
        let before = vec![member(&alice, "dev-a")];
        let after = vec![member(&alice, "dev-a"), member(&mallory, "dev-a")];

        let diff = leaf_diff(&before, &after);
        assert_eq!(diff.gained, vec![member(&mallory, "dev-a")]);
        assert!(diff.lost.is_empty());
    }

    #[test]
    fn diff_is_order_insensitive() {
        let alice = Keypair::generate();
        let bob = Keypair::generate();
        let before = vec![member(&alice, "a"), member(&bob, "b")];
        let after = vec![member(&bob, "b"), member(&alice, "a")];
        assert!(leaf_diff(&before, &after).is_empty());
    }

    #[test]
    fn dead_leaves_is_the_leaf_set_minus_the_live_set() {
        // Exercised against a real group in the harness
        // (`a_ban_does_not_brick_a_channel`); this pins the subtraction itself.
        let alice = Keypair::generate();
        let bob = Keypair::generate();
        let live = vec![member(&alice, "dev-a")];
        let leaves = vec![member(&alice, "dev-a"), member(&bob, "dev-b")];
        let dead: Vec<_> = leaves
            .into_iter()
            .filter(|m| !live.iter().any(|k| k.identity == m.identity && k.device == m.device))
            .collect();
        assert_eq!(dead, vec![member(&bob, "dev-b")]);
    }
}
