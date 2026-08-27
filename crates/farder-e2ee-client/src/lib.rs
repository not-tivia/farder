//! `farder-e2ee-client` — transport-agnostic E2EE control-plane logic for
//! Farder's sealed channels (mesh rung 2, sub-project 4).
//!
//! This crate is the headless "vertical": it holds the MLS steward/group logic
//! and talks to a server only through the [`E2eeTransport`] trait, so it can be
//! driven both by the real Tauri client (sub-project 4b) and by the in-process
//! test harness (sub-project 4a) against the *shipped* code rather than a
//! reimplementation. See `docs/superpowers/plans/2026-08-26-mesh-rung2-sub4a-sealed-vertical.md`
//! and the rung-2 E2EE design spec.
//!
//! This crate deliberately has **no networking dependency** (no `quinn`, no
//! `tauri`): the transport is supplied by the caller behind the trait.

pub mod chain;
pub mod channel;
pub mod channel_key;
pub mod commit;
pub mod join;
pub mod transport;

#[cfg(test)]
pub mod testing;

pub use chain::{build_next_event, event_now_secs, Actor, ChainState};
pub use channel::{
    bootstrap_group, channel_group_id, create_e2ee_channel, publish_key_package,
    persist_store_instance_hash, read_store_instance_hash, ChannelSpec, CommitSubmitted,
    CreateChannelOutcome, E2eeError, KeyPackageOutcome, KEY_PACKAGE_LIFETIME_LOG_POSITIONS,
};
pub use channel_key::{validate_log_server_id, ChannelKey};
pub use commit::{
    add_member, process_incoming_commit, AddMemberOutcome, DeclaredCommit, DeviceCertResolver,
    IncomingCommitOutcome, StewardContext,
};
pub use farder_mls::group::JoinInfo;
pub use join::{
    confirm_leaf, create_joiner_store, fetch_pending_welcomes, join_channel, resume_store,
    LeafConfirmation, PendingWelcome, SendEligibility,
};
pub use transport::{E2eeTransport, EventAccepted, TransportError, Welcomes};
