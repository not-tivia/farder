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
