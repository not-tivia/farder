# Mesh Rung 2 — Sub-project 4a: Sealed Vertical (headless) + Two-Client Harness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Light up the dormant E2EE control plane end to end, headlessly. Two independent identities, in one process, against an in-process server: owner creates an E2EE channel, both publish KeyPackages, owner bootstraps the MLS group and adds the joiner, the joiner confirms its leaf, both exchange sealed messages that the other actually decrypts — and an observation test proves no plaintext reached any table on any path. **No GUI, no Tauri command layer, no React.** Everything in 4a is provable in WSL; the GUI surface is sub-project 4b.

**Scope boundary (why 4a/4b):** the spec's sub-project 4 bundles the vertical with its UI. They are split here on the verifiability line, following the Rung-1 sub-3a/3b and sub-4a/4b precedent: **4a is 100% testable on this machine; 4b needs the owner's Windows box.** Bundling them would make the whole sub-project un-mergeable until a manual run, which is exactly the coupling the harness exists to remove.

**Tech Stack:** Rust. `farder-mls` (real OpenMLS), `farder-crypto` (`event_log`, `event_log_state`), `farder-protocol`, `farder-server` (in-process), `quinn`/`rustls`/`rcgen` for the harness transport. **One new workspace crate** (below). No new third-party dependencies.

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` (rev 2), sub-project 4.

**Baseline measured on this branch @ `abbbca1` (2026-08-26):** `cargo test --workspace` = **764 passed, 22 binaries, 0 failed**. Keep it green at every commit.

---

## Authority note: plan against the code, not against memory or the spec's prose

Every claim below was read out of the working tree during recon on 2026-08-26 and is
cited. Where the spec's prose disagrees with the code, **the code wins**.

### A1. The three decisions this plan makes

**DECISION 1 — the vertical logic lives in a NEW workspace crate, not in `client/src-tauri`.**

The spec says the steward/MLS logic should live "in the client *crate* as plain library
code … driven by a headless harness … so it is testable in WSL". That is not achievable
as written: `client/src-tauri/Cargo.toml:1` opens with its **own `[workspace]`**, so
`farder-client` is **not** a member of the root workspace (root members are the eight
`crates/*`, `Cargo.toml:3-12`). Root integration tests therefore **cannot link it**. A
harness under `tests/` would be exercising a *reimplementation* of the vertical, not the
shipped code — the harness would go green while the real client stayed broken.

So the vertical goes in **`crates/farder-e2ee-client`**, a root-workspace member that both
`client/src-tauri` and `tests/` can depend on. The Tauri command layer (4b) becomes a thin
wrapper. This is what makes the harness meaningful.

**DECISION 2 — the crate is transport-agnostic behind an `E2eeTransport` trait.**

The vertical needs to submit events and fetch (Welcomes, KeyPackages, history), but the
real connection lives in `client/src-tauri` (`bridge::send_request`). So the crate defines
a trait with exactly the calls it needs; the Tauri client implements it over its real
connection in 4b, and the harness implements it over the test QUIC connection. This mirrors
the project's existing seam idiom (`DisplayBackend`, `PresenceSource`, `LinkFetcher`).

**DECISION 3 — the server's v2 emit sites are in 4a's scope.**

Sub-3's final review recorded "v2-only events have no emit sites (sub-4's scope, stated
dormant)" as an explicit carry-forward. Confirmed in the code: `SealedMessage`,
`SealedMessageEdited`, `MessageTombstoned`, `MlsControlEvent` and `ChannelCreatedV2` are
defined (`farder-protocol/src/server.rs:876-889`), classified v2-only by
`event_requires_v2` (`server.rs:900-959`) and gated by `may_receive`
(`farder-server/src/connection.rs:1543-1555`) — but **nothing emits them**, and
`handlers.rs:2454-2457` says so in a comment. Without emit sites there is no live sealed
delivery at all and every client would have to poll `FetchHistoryV2`. 4a adds them.

> **Consequence for the owner:** 4a touches `farder-server`, so its eventual runtime test
> needs a **sidecar rebuild**, not just a frontend reload.

### A2. Load-bearing facts the implementer must not re-derive

1. **"Accepted" does not mean "took effect."** A commit submitted at the wrong epoch is
   *accepted* as `Authorized::StaleCommitNoOp` (`event_log_state.rs:1061-1063`): the chain
   head and `log_pos` advance, **zero MLS state changes**. The client must re-read
   `LogState::mls_current_epoch` after every commit rather than trusting `EventAccepted`.
2. **`stale-epoch` is a bare string.** `ServerResponse::Error { reason: "stale-epoch" }`,
   NOT prefixed with `"event rejected:"` like every other SubmitEvent rejection
   (`handlers.rs:2301-2307`). It is returned **before** signature/authz validation,
   deliberately. The resync loop keys on this exact string.
3. **Two client-side gates on every received commit, not one.**
   `MlsChannelGroup::process_commit_checked` (`farder-mls/src/group.rs:435`) refuses to
   merge a commit whose declared adds/removes/tree-hash mismatch reality — but it **cannot**
   distinguish a genuine leaf from an impostor leaf that cloned a real member's credential
   bytes. Every `ProcessedCommit::actual_adds` entry must **also** pass
   `credential::verify_leaf_binding` against a log-valid `DeviceCert`
   (`group.rs:430-434`; the crate's own test `impostor_add_passes_declared_check_but_fails_leaf_binding_on_actual_leaf`
   at `group.rs:1216-1277` proves the gap). Never call plain `process_commit`.
4. **Opening a sealed message consumes that generation's key — including on failure.**
   Never retry `open_message` on the same bytes (`docs/modules/farder-mls.md:294-297`).
   A decrypt failure is terminal for that message and must render fail-closed.
5. **A fresh E2EE channel is not addable until its creator commits once.** The bootstrap
   commit is creator-only (`event_log_state.rs:1076-1089`) and is what confirms the
   creator's own leaf (`event_log_state.rs:1661-1667`).
6. **Sealed sends require a CONFIRMED leaf.** `check_sealed_send`
   (`event_log_state.rs:1403-1462`) rejects with
   `"sealed content author does not hold a confirmed leaf"` until `MlsLeafConfirmed` lands.
7. **`NegotiateProtocol` must come first**, and is the only v2 request allowed
   pre-membership (`handlers.rs:504-516`). Absent negotiation the connection is treated as
   v1 (`handlers.rs:520-533`), v2 requests return `UPGRADE_REQUIRED`, and **v1
   `GetServerInfo` silently omits every non-plaintext channel** (`handlers.rs:1378-1386`) —
   so a client that forgets to negotiate sees no sealed channels at all and shows no error.
   Negotiation is per-connection and cleared on disconnect (`connection.rs:719-726`) — a
   reconnecting client must negotiate again.
8. **`FetchWelcomes` pagination:** feed the returned `next_accept_seq` back as
   `since_accept_seq` and loop while `more == true` (`event_ingest.rs:810-851`). The cursor
   advances past **non-matching** rows too, so never restart from 0.
9. **E2EE channel ids have a floor:** `E2EE_CHANNEL_ID_FLOOR = 1 << 32`
   (`event_log.rs:69`). The client chooses the id.
10. **E2EE channels are created by a LOG event, not by `CreateChannel`.** The owner submits
    `ChannelCreated { class: E2ee }` and the server materializes the row inside the accept
    transaction (`materialize_channel_created`). The legacy `CreateChannel` request has no
    class field and produces a plaintext row.
11. **Caps:** content ≤ 8000 chars and ≤ 32 KiB pre-seal (`farder-mls/src/lib.rs:30,33`),
    ciphertext ≤ 45 KiB, ≤ 10 attachments (`event_log.rs:44,61`), `core.timestamp` ≤ 300 s
    ahead of server time (`event_log.rs:75`).
12. **`FarderMlsStore::create` refuses an existing path**, and `resume` needs the
    `store_instance_hash` persisted somewhere durable; all resume errors are **terminal**
    (`store.rs:144-204`). 4a persists the hash and fails loudly; re-provisioning is sub-5's.

### A3. What 4a deliberately does NOT do

Deferred to **4b**: Tauri commands, channel-class creation UI, the encrypted-composer
affordance, fail-closed rendering, the five UI states, local store + client-side search,
theming in all three `theme.css` files.
Deferred to **sub-5**: steward drift loop, rekey cadence, multi-device, `DeviceRevoked`,
group reset, store re-provisioning.
Deferred to **sub-6/7**: encrypted attachments, PIN-wrapped local history.
`AttachmentCap`s on `MessagePostedE2ee` are carried as an empty vec in 4a.

---

## Task 1 — New crate `farder-e2ee-client` + the transport seam

- [ ] Create `crates/farder-e2ee-client` (edition/lints matching a sibling crate); add to root `Cargo.toml` members.
- [ ] Deps: `farder-mls`, `farder-crypto`, `farder-protocol`, `anyhow`, `serde`, `rmp-serde`. **No `quinn`, no `tauri`.**
- [ ] Define `trait E2eeTransport` with exactly: `submit_event(&Event) -> Result<EventAccepted, TransportError>`, `fetch_welcomes(channel_id: Option<u64>, since: u64) -> Result<Welcomes>`, `fetch_key_packages(member, device) -> Result<Vec<Vec<u8>>>`, `fetch_history_v2(channel_id, before_id, limit) -> Result<Vec<MessageInfoV2>>`.
- [ ] `TransportError` MUST expose a `is_stale_epoch()` predicate matching the bare `"stale-epoch"` string (fact A2.2) — the resync loop depends on it and a substring match on `"event rejected"` would miss it.
- [ ] Define `ChannelKey { log_server_id, channel_id }` and the on-disk layout helper: MLS store at `servers/{log_server_id}/mls/{channel_id}.sqlite`, instance hash beside it (consistent with `device_state.json`'s `servers/{id}/` convention).
- [ ] Reuse `device::validate_server_id`'s hex-only path-traversal guard shape for any path built from a server-supplied id.

**Verify:** `cargo build --workspace` clean; crate has zero `quinn`/`tauri` in its dependency tree (`cargo tree -p farder-e2ee-client | grep -c -E 'quinn|tauri'` = 0).

## Task 2 — Channel creation, KeyPackage publication, bootstrap commit

- [ ] `create_e2ee_channel(...)`: pick `channel_id >= E2EE_CHANNEL_ID_FLOOR`, submit `ChannelCreated { class: E2ee, .. }`, then `FarderMlsStore::create` + `MlsChannelGroup::create`, persist `store_instance_hash`.
- [ ] `publish_key_package(...)`: `credential::generate_key_package` → `tls_serialize_detached` → `MlsKeyPackagePublished { key_package, store_instance_hash, expires_at_log_pos }`. `expires_at_log_pos` must be `> log_pos` or the fold rejects.
- [ ] `bootstrap_group(...)`: creator-only first commit at epoch 0 (fact A2.5). Emit `MlsCommit` with the real `CommitOutcome` fields (`prev_epoch_authenticator`, `post_epoch_authenticator`, `post_tree_hash`, `epoch`).
- [ ] After every commit submit, **re-read `mls_current_epoch` and assert it advanced** (fact A2.1). A `StaleCommitNoOp` must surface as a distinct, non-silent outcome.

**Verify:** unit tests with an in-memory fake transport: channel created at/above the floor; KeyPackage round-trips through `decode_key_package`; bootstrap advances epoch 0→1 and confirms the creator's leaf; a deliberately-stale commit is reported as a no-op, **not** as success.

## Task 3 — Join: Welcome fetch, group join, leaf confirmation

- [ ] `fetch_pending_welcomes(...)`: paginate per fact A2.8 — loop on `more`, feeding `next_accept_seq`. Assert in a test that a Welcome sitting behind >500 unrelated rows is still reached.
- [ ] `join_channel(...)`: `join_from_welcome` → obtain `JoinInfo { epoch, tree_hash }` → submit `MlsLeafConfirmed { generation, epoch, tree_hash, store_instance_hash }`, authored **by the joining device** (`event_log_state.rs:1271-1277`).
- [ ] The confirmed `tree_hash` must equal the cited epoch's commit `post_tree_hash`; on mismatch, fail loudly rather than submitting a doomed event.
- [ ] Do not attempt a sealed send before confirmation succeeds (fact A2.6); expose `can_send()` derived from `leaves_confirmed`.

**Verify:** joiner reaches epoch N, confirms, and `leaves_confirmed` contains its `(identity, device)`; a pre-confirmation send attempt is refused locally with a typed error, not a server round-trip.

## Task 4 — Steward add + the two receive-side gates

- [ ] `add_member(...)`: `fetch_key_packages` → `decode_key_package` (fails closed on non-farder credentials) → `add_members` → submit `MlsCommit` with declared adds → submit `MlsWelcome { commit: <commit event hash>, for_member, for_device, welcome }`.
- [ ] `process_incoming_commit(...)`: **`process_commit_checked` only** (fact A2.3), passing the declared adds/removes/`post_tree_hash` from the `MlsCommit` event.
- [ ] **Then** `verify_leaf_binding` on every `ProcessedCommit::actual_adds` entry against a `DeviceCert` resolved from the log. A failure must reject the commit and surface an equivocation-class warning — never silently accept.
- [ ] Process commits **in order**; a gap must block rather than skip.

**Verify:** a test where a hostile commit declares Alice but actually adds an impostor leaf carrying Alice's credential bytes — `process_commit_checked` passes, `verify_leaf_binding` **fails**, and the client rejects. This test is the whole point of the task; it must fail if either gate is removed.

## Task 5 — Sealed send and receive

- [ ] `send_sealed(...)`: build `MessageEnvelope` → `check_preseal_limits` → `seal_message` → `MessagePostedE2ee { generation, epoch, ciphertext, reply_to, attachments: vec![], authz_head }` citing the group's **current** epoch.
- [ ] `receive_sealed(...)`: `open_message` exactly once per ciphertext (fact A2.4). Return a `SealedOutcome::{ Decrypted(MessageEnvelope), Undecryptable { reason } }` — **never** a plaintext fallback, and never a retry.
- [ ] Reply support: carry `reply_to: Option<EventRef>` (sub-3 already maps event-hash → row id server-side), closing the `MessageInput.tsx:291` TODO at the protocol level for 4b to consume.

**Verify:** round-trip between two real `MlsChannelGroup`s; a tampered ciphertext yields `Undecryptable` and does **not** panic (the crate contains the AEAD `debug_assert` panic via `catch_unwind`); a second `open_message` on the same bytes is never attempted (assert by construction/API shape, not by comment).

## Task 6 — Stale-epoch resync, bounded

- [ ] On a `stale-epoch` rejection: fetch and process outstanding commits → re-seal at the new epoch → retry.
- [ ] **Bound it at 3 unproductive attempts**, then surface an equivocation warning (spec: "after 3 unproductive retries surfaces the equivocation warning instead of looping silently"). "Unproductive" = the epoch did not advance between attempts.
- [ ] The loop must not be able to spin when the epoch is advancing but our send keeps losing the race — cap total attempts regardless.

**Verify:** a test transport that rejects with `"stale-epoch"` a fixed number of times then accepts; and a pathological one that always rejects — the second must terminate with the equivocation outcome, not hang. Given this codebase's recurring "over-conservative guard creates an unexitable state" bug class, explicitly assert termination.

## Task 7 — Server v2 emit sites

- [ ] In the `SubmitEvent` accept path (`handlers.rs:2390-2468`), emit alongside the existing plaintext broadcasts: `SealedMessage` and `SealedMessageEdited` to `EventTarget::Subscribers(channel_id)`; `MlsControlEvent` for the MLS control variants; `ChannelCreatedV2` for an E2EE `ChannelCreated`.
- [ ] Do **not** widen `Subscribers` semantics: `Subscribe` is a permission boundary and sealed delivery rides it (`subscriptions.rs:1-49`). Merging a class gate into `channel_visible` would silently break the whole rung — see the sub-3 merge note.
- [ ] Confirm `may_receive` already withholds all five from v1 connections; add a test rather than assuming.
- [ ] Remove/replace the now-stale comment at `handlers.rs:2454-2457`.

**Verify:** a v2-negotiated subscriber receives `SealedMessage`; a v1 (non-negotiated) subscriber on the same channel receives **nothing**; an E2EE channel remains subscribable (do not regress `an_e2ee_channel_is_still_subscribable`).

## Task 8 — The headless two-client harness

- [ ] New root integration test `tests/e2ee_two_client.rs`, registered as a `[[test]]` in the root `Cargo.toml` (mirroring `e2e_server`, lines 38-40).
- [ ] Add `farder-mls` and `farder-e2ee-client` to the root `[dev-dependencies]` (currently `farder-mls` is only in `[workspace.dependencies]`).
- [ ] Reuse the `e2e_server.rs` setup verbatim in shape: in-process server accept loop, `connect_and_auth` for two `Keypair`s, mesh log join (`DeviceAuthorized` → `ResolveInvite` → `MemberJoined`), then `NegotiateProtocol { client_version: 2 }` on **both** connections.
- [ ] Drive both identities through `farder-e2ee-client` over an `E2eeTransport` impl backed by the real QUIC connection — so the harness exercises the shipped vertical, per Decision 1.
- [ ] **Full-path test:** owner creates E2EE channel → both publish KeyPackages → owner bootstraps → owner adds joiner → joiner fetches Welcome, joins, confirms → owner sends sealed → **joiner decrypts and asserts the exact plaintext** → joiner replies sealed → owner decrypts.
- [ ] **Observation test:** factor `assert_no_plaintext_anywhere` out of `e2ee_observation.rs` into a shared test helper (it is currently a private fn in that file) and assert the message plaintext appears in **no** table. Keep its `the_observer_finds_a_needle_that_is_really_there` self-check alive so the assertion cannot go vacuously green.
- [ ] **Negative test:** a third identity that is a server member but **not** in the MLS group cannot decrypt the ciphertext it can fetch.

**Verify:** all three tests green; `cargo test --workspace` still green at **≥764** plus the new tests.

---

## Gates (every task)

- `cargo test --workspace` green (baseline 764 — never fewer).
- `cargo build --workspace` with no new warnings.
- `cargo clippy -p farder-e2ee-client -- -D warnings` clean (new crate starts clean and stays clean; do NOT put `-D warnings` on the pre-existing crates — that forced a whole-crate cleanup sweep on sub-2).
- Client crate builds separately (`cd client/src-tauri && cargo build`) once it depends on the new crate — `cargo build --workspace` does NOT cover it.
- Check `git ls-files --eol` after any scripted edit: python `io.open(p,'w')` strips CRLF and destroyed blame on a wire-protocol file during sub-3.

## Review discipline

Per the standing rule: verify each load-bearing guard by **breaking it and watching its
test fail**, in a scratch `git worktree` under /tmp — never the live checkout. The guards
that must be proven this way in 4a: the `verify_leaf_binding` second gate (Task 4), the
stale-epoch retry bound (Task 6), the v1 withholding of sealed events (Task 7), and the
no-plaintext observer (Task 8). A fix to any of these gets its own adversarial review pass —
on sub-2, half the findings came from fixing the first half.
