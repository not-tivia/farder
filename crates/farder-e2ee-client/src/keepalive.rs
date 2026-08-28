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

use farder_mls::group::MlsChannelGroup;

use crate::chain::{Actor, ChainState};
use crate::channel::E2eeError;
use crate::join::SendEligibility;
use crate::rekey::{rekey_channel, RekeyContext};
use crate::sealed::{send_sealed, SealContext, SealedSendOutcome};
use crate::transport::E2eeTransport;

/// What [`send_sealed_keepalive`] had to do to get the message out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepaliveAction {
    /// The send succeeded directly; the channel was healthy.
    SentDirectly,
    /// The freshness ceiling had sealed the channel. A rekey cleared it and the
    /// message went out on the retry.
    RekeyedThenSent,
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
/// - A **pending-removals** rejection is NOT handled here: discharging drift
///   needs the dead-leaf set, which only the caller can derive (from the
///   `DeviceRevoked` or the ban). It surfaces unchanged so the caller can route
///   it to [`crate::drift::discharge_drift`] and tell the user what is happening.
/// - Every other error surfaces unchanged, including the divergence class — a
///   rekey that is itself rejected leaves the local group one epoch ahead
///   (finding F1), and this function must never paper over that.
pub async fn send_sealed_keepalive<T: E2eeTransport + Sync>(
    transport: &T,
    actor: &Actor<'_>,
    chain: &mut ChainState,
    ctx: &SealContext<'_>,
    rekey_ctx: &RekeyContext<'_>,
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
            rekey_channel(transport, actor, chain, rekey_ctx, group).await?;
            let send = send_sealed(transport, actor, chain, ctx, group, eligibility).await?;
            Ok(KeepaliveOutcome {
                send,
                action: KeepaliveAction::RekeyedThenSent,
            })
        }
        Err(e) => Err(e),
    }
}
