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

pub mod cert;
pub mod chain;
pub mod channel;
pub mod channel_key;
pub mod commit;
pub mod device;
pub mod drift;
pub mod join;
pub mod rekey;
pub mod reset;
pub mod resync;
pub mod revoke;
pub mod sealed;
pub mod transport;

#[cfg(test)]
pub mod testing;

pub use cert::{build_cert_resolver, resolve_device_cert, VerifiedCertResolver};
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
pub use device::{add_own_device, authorize_device, AddOwnDeviceOutcome, DeviceAuthorizedOutcome, OwnDeviceContext};
pub use drift::{
    dead_leaves_from_revocation, discharge_drift, DriftDischargeContext, DriftDischargeOutcome,
};
pub use farder_mls::group::JoinInfo;
pub use join::{
    confirm_leaf, create_joiner_store, fetch_pending_welcomes, join_channel, resume_store,
    LeafConfirmation, PendingWelcome, SendEligibility,
};
pub use rekey::{
    rekey_channel, rekey_permitted_by_rate_rule, should_rekey, HoldReason, RekeyCadence,
    RekeyContext, RekeyDecision, RekeyOutcome, RekeyTrigger, REKEY_SEALED_SEND_INTERVAL,
    REKEY_WALL_CLOCK_SECS,
};
pub use reset::{join_reset, member_live_leaves, reset_group, ResetContext, ResetOutcome};
pub use revoke::{revoke_device, RevokeOutcome};
pub use resync::{
    fetch_mls_control_exhaustive, send_sealed_resync, ResyncOutcome, ResyncRequest,
    MAX_TOTAL_RESYNC_ATTEMPTS, MAX_UNPRODUCTIVE_RESYNC_ATTEMPTS,
};
pub use sealed::{
    receive_sealed, send_sealed, SealContext, SealedOutcome, SealedSendOutcome,
};
pub use transport::{E2eeTransport, EventAccepted, MlsControl, TransportError, Welcomes};
