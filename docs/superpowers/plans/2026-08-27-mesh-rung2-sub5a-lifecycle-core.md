# Mesh Rung 2 — Sub-project 5a: Lifecycle Core (headless) — Implementation Plan

> **For agentic workers:** superpowers:subagent-driven-development. Steps use `- [ ]`.

**Goal:** light up the dormant lifecycle machinery — rekey, device revocation, group reset, multi-device, drift discharge, store re-provisioning — in the `farder-e2ee-client` crate and the server, headlessly harness-tested. This is sub-5a; sub-5b is the GUI wrapper (transparency notices, revoke/reset actions).

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` sub-project 5. Baseline: `main` @ `342164c`, 839 workspace tests green.

## Scope decision (verified against recon)

Sub-5 splits on the verifiability line, exactly like 4a/4b: **5a is pure crate + server, fully harness-testable in WSL; 5b is the GUI, Windows-tested.** The hard parts — rekey cadence, the drift loop, group reset, revocation — are all pure logic that belongs in the crate.

**F2 (KeyPackage expiry / `log_pos`) is DEFERRED, not fixed here.** The wedge it describes (a single device publishing 10 never-consumed KPs) is narrow: re-provisioning mints a *fresh* device with its own fresh 10 slots, so multi-device/re-provisioning (this sub-project) does not hit it. Fixing it needs a new `log_pos` protocol surface, which is not worth a wire change inside a lifecycle sub-project. Recorded as a carry-forward.

## Authority note — the fold rules (cite, don't re-derive)

1. **Rekey = `self_update` = an `MlsCommit` with empty adds/removes** — no fold distinction, legal under confirmed-leaf + commit-rate rules. A rekey must satisfy `commit_discharges_drift` (it won't — empty) OR `ceiling_demands_rekey()` (events_since_last_commit+50 >= 500) OR first commit OR `epoch >= last + gap` where `gap = min(4, committing_identities)`. So a client rekey is gated; the ceiling override guarantees eventual rekey (`event_log_state.rs:1187-1203`, `:267-270`).
2. **`DeviceRevoked`** is owner-or-owning-identity (`:996-1010`), inserts into `revoked_devices` only; the leaf becomes drift lazily via `pending_removals`, which SEALS sends until a remove-commit discharges it (`:576-587`, `:1445-1448`). The device's chain is frozen (any further event from a revoked device is rejected, `:701-707`).
3. **`MlsGroupReset`** is owner-only, `new_generation == generation + 1`, exact-cover over `member_leaf_set − resetter's own device`, rate-limited (`RESET_MIN_CHANNEL_EVENTS = 1000`), incomplete-reset-exempt (`:1342-1394`). After accept, `reset_incomplete()` seals the channel until every `reset_welcomed` leaf confirms (`:261-263`, `:1454-1460`).
4. **Device cap 8** enforced at `DeviceAuthorized`; **self-add rule**: once an identity holds a confirmed leaf, only that identity adds its further devices (`:1136-1145`).
5. **`MlsLeafConfirmed`** is the confirmation wall: the joining device confirms a pending leaf; tree-hash must match the cited commit OR the reset's declared hash (`:1279-1316`).
6. **`DeviceRevoked` is NOT fetchable or broadcast today** — `FetchDeviceCerts` returns only `DeviceAuthorized` (`event_ingest.rs:996-999`), and the `MlsControlEvent` broadcast has no `DeviceRevoked` arm (`handlers.rs:2514-2521`). This is the gap that blocks revocation-aware clients.
7. **`MlsGroupReset` IS fetchable** (in `fetch_mls_control`'s list, `event_ingest.rs:895`) but the client crate's `apply_commits` and the Tauri `process_mls_control_events` SKIP it — no client handles a reset today.
8. **Store re-provisioning**: `StoreResumeError` (InstanceMismatch/MissingInstanceId/Io) is terminal; `create` refuses existing paths, `resume` never recreates. Re-provisioning = self-`DeviceRevoked` + a FRESH store path (`store.rs:79-90`, `144-205`).

---

## Tasks

### Server/protocol (small, new variants only — never mutate a shipped struct)

- [ ] **S1 — make `DeviceRevoked` fetchable + broadcast.** Widen `fetch_device_certs` to also return an identity's `DeviceRevoked` events (so a resolver can filter revoked devices), and add a `DeviceRevoked` arm to the `MlsControlEvent` broadcast so peers learn live. NEW response shape where needed; document the mixed-payload fetch in the protocol doc. The client resolver becomes revocation-aware in C1.
- [ ] **S2 — a liveness/revoked verdict in the cert fetch** (or a sibling `FetchDeviceStatus`). Decide the cleanest: either the widened `DeviceCerts` (client folds `DeviceAuthorized`/`DeviceRevoked` itself) or a server-computed verdict. Prefer the client-fold approach (no server trust expansion; matches "server is blind to MLS but stores the log"). Justify.

### Client crate (headless, harness-tested)

- [ ] **C1 — revocation-aware cert resolver.** `resolve_device_cert` folds the identity's `DeviceRevoked` events and rejects a revoked device's cert; check `DeviceCert.expires_at` against a clock bound. Update `VerifiedCertResolver`/`build_cert_resolver`. This closes the 4a honesty gap.
- [ ] **C2 — rekey (`self_update`) + cadence.** A `rekey_channel` fn that runs `self_update`, submits the empty-adds/removes `MlsCommit`, and is gated: skip if `pending_removals` non-empty (drift must be discharged first) or if the commit-rate rule would reject; surface `ceiling_demands_rekey` as the "must rekey" trigger. Termination-bounded like the 4a resync.
- [ ] **C3 — drift discharge (remove dead leaves).** Detect `pending_removals`; a remaining confirmed member authors a `remove_members` commit listing the dead leaves. Handle the race: multiple members discharge at once → one wins the epoch CAS, losers get `stale-epoch` and back off (idempotent, no spin).
- [ ] **C4 — `DeviceRevoked` emission.** Self-revoke (I lost a device / store died) and owner-revoke (revoke a member's device). Wrap the event submission with the chain-advance pattern; the store-re-provision path (C7) calls the self-revoke form.
- [ ] **C5 — `MlsGroupReset` emission (owner) + handling (member).** Owner: build the next-generation group, Welcome every current member (exact cover), submit `MlsGroupReset`. Member: on receiving a reset, `join_from_welcome` at the new generation and `confirm_leaf` against the reset's declared tree hash.
- [ ] **C6 — multi-device self-add.** The "add my own second device" path (identity signs a DeviceAuthorized for the new device, publishes a KeyPackage, then self-adds per the self-add rule). Bounded by the device cap.
- [ ] **C7 — store re-provisioning.** On a terminal `StoreResumeError`, self-`DeviceRevoked`, mint a fresh store path + instance hash, and re-publish. The recovery path that makes the terminal error non-fatal.

### Harness (the named deliverable)

- [ ] **H1 — lifecycle harness.** Extend the two-client harness (or a new `e2ee_lifecycle.rs`) to prove, observationally: ban → send gate engages → rekey → captured old state cannot decrypt new traffic; ghost-Welcome drift self-heals; stale channel blocks then unblocks after rekey; device-loss rejoin; partial reset refused. Reuse the 4a harness's server + `assert_no_plaintext_anywhere`.

## Gates
- `cargo test --workspace` ≥ 839, never fewer.
- `cargo build --workspace` no new warnings; `cargo clippy -p farder-e2ee-client --no-deps -- -D warnings` clean.
- Client crate builds separately; `cd client && npx tsc --noEmit` (5a touches no frontend, but confirm).
- `git ls-files --eol` after scripted edits.

## Review discipline
The standard rules: verify each load-bearing guard by breaking it and watching its test fail (scratch worktree); fixes to security rules get their own adversarial pass. The load-bearing items here are the rekey cadence gate (C2), the drift-discharge race (C3), the revocation-aware resolver (C1), and the reset completeness (C5).

## Carry-forwards (recorded, not done)
- F2: KeyPackage expiry still effectively infinite; needs a `log_pos` surface (deferred).
- "channel needs a key refresh" UI state (5b, once C2 exposes the ceiling/staleness signal).
- The owner's Windows run for 5b.
