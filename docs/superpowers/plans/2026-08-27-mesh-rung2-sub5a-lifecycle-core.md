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

- [x] **S1 — make `DeviceRevoked` fetchable + broadcast.** Widen `fetch_device_certs` to also return an identity's `DeviceRevoked` events (so a resolver can filter revoked devices), and add a `DeviceRevoked` arm to the `MlsControlEvent` broadcast so peers learn live. NEW response shape where needed; document the mixed-payload fetch in the protocol doc. The client resolver becomes revocation-aware in C1.
- [x] **S2 — a liveness/revoked verdict in the cert fetch** (or a sibling `FetchDeviceStatus`). Decide the cleanest: either the widened `DeviceCerts` (client folds `DeviceAuthorized`/`DeviceRevoked` itself) or a server-computed verdict. Prefer the client-fold approach (no server trust expansion; matches "server is blind to MLS but stores the log"). Justify.

### Client crate (headless, harness-tested)

- [x] **C1 — revocation-aware cert resolver.** `resolve_device_cert` folds the identity's `DeviceRevoked` events and rejects a revoked device's cert; check `DeviceCert.expires_at` against a clock bound. Update `VerifiedCertResolver`/`build_cert_resolver`. This closes the 4a honesty gap.
- [x] **C2 — rekey (`self_update`) + cadence.** A `rekey_channel` fn that runs `self_update`, submits the empty-adds/removes `MlsCommit`, and is gated: skip if `pending_removals` non-empty (drift must be discharged first) or if the commit-rate rule would reject; surface `ceiling_demands_rekey` as the "must rekey" trigger. Termination-bounded like the 4a resync.
- [x] **C3 — drift discharge (remove dead leaves).** Detect `pending_removals`; a remaining confirmed member authors a `remove_members` commit listing the dead leaves. Handle the race: multiple members discharge at once → one wins the epoch CAS, losers get `stale-epoch` and back off (idempotent, no spin).
- [x] **C4 — `DeviceRevoked` emission.** Self-revoke (I lost a device / store died) and owner-revoke (revoke a member's device). Wrap the event submission with the chain-advance pattern; the store-re-provision path (C7) calls the self-revoke form.
- [x] **C5 — `MlsGroupReset` emission (owner) + handling (member).** Owner: build the next-generation group, Welcome every current member (exact cover), submit `MlsGroupReset`. Member: on receiving a reset, `join_from_welcome` at the new generation and `confirm_leaf` against the reset's declared tree hash.
- [x] **C6 — multi-device self-add.** The "add my own second device" path (identity signs a DeviceAuthorized for the new device, publishes a KeyPackage, then self-adds per the self-add rule). Bounded by the device cap.
- [x] **C7 — store re-provisioning.** On a terminal `StoreResumeError`, self-`DeviceRevoked`, mint a fresh store path + instance hash, and re-publish. The recovery path that makes the terminal error non-fatal.

### Harness (the named deliverable)

- [x] **H1 — lifecycle harness.** Extend the two-client harness (or a new `e2ee_lifecycle.rs`) to prove, observationally: ban → send gate engages → rekey → captured old state cannot decrypt new traffic; ghost-Welcome drift self-heals; stale channel blocks then unblocks after rekey; device-loss rejoin; partial reset refused. Reuse the 4a harness's server + `assert_no_plaintext_anywhere`.

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
- **A single-device identity that loses its MLS store cannot self-recover.** C7's
  recovery mints a NEW leaf (MLS ratchet state lives in the store and cannot be
  rebuilt from the log), and the self-add rule requires an existing confirmed
  device of the same identity to author it. With only one device there is none,
  so the escape is an owner `MlsGroupReset` (C5). Documented in `reprovision.rs`;
  5b must SAY this in the UI rather than leaving the user at a dead channel.
- **Diverged-group recovery costs a fresh device key.** F1 imagined re-adding a
  fresh leaf for the SAME device; C7 re-provisions instead (revoke + fresh key +
  fresh store). Cheap enough — the fold frees a revoked device's slot, so the
  8-device cap is not consumed — but it churns the device identity, and 5b should
  surface it as "this device was re-provisioned", not silently.

---

## Findings recorded during the build (controller)

### F1 (sub-5a) — a rejected own-commit diverges local state, and there is NO recovery path yet

`self_update`/`add_members`/`bootstrap_group` all merge locally *before* the submit is
accepted. So a rejected own-commit — `stale-epoch` (epoch CAS) OR the commit-rate rule —
leaves the local `MlsChannelGroup` one epoch AHEAD of the server, with no rollback (4a's
finding F4 already noted this for `LeafBindingFailure`; it is the same class for the
commit-rate/stale rejections). C2 added rekey (another own-commit) and C3 will add
drift-discharge (remove, another own-commit), so the surface keeps growing.

**There is currently no client-side "rebuild the group from the log" recovery.** A
divergent group can only be fixed by re-joining (getting a fresh Welcome for a new
generation) — which is precisely the spec's "MLS-state-loss-without-device-loss recovery"
item. This must land in C7 (store re-provisioning) as a REAL recovery path, not just the
"self-DeviceRevoked + fresh device" terminal path: a device whose MLS store diverged (but
whose device key is fine) should re-join the channel without revoking itself.

**Carry-forward into C7:** implement `recover_diverged_group` = detect divergence (own
commit rejected), self-remove the stale leaf, and re-add a fresh leaf for the same device
(via a new KeyPackage + Welcome), or — if the group is beyond local repair — re-provision.
Until then, `RekeyRateLimited`/`StaleEpochDiverged` are effectively terminal for the local
group, which is unacceptable for a *lifecycle* sub-project.

### F2 (sub-5a, found by H1) — `reprovision_device` does not recover a multi-identity channel

The harness proved the recovery path's happy case (owner-only multi-device, gap=1) but
exposed that a genuine 2-identity recovery is broken: when a device self-revokes, its
confirmed leaf becomes drift (`pending_removals`), which SEALS the channel. The subsequent
`add_own_device` is an add-only commit — it does NOT discharge that drift (adds don't match
`pending_removals`), and in a channel with ≥2 committing identities it falls under the
commit-rate rule and is refused. So a 2-identity device-loss recovery gets stuck sealed.

**Fix required:** `reprovision_device` must discharge the old leaf's drift (a remove-commit
for the dead `(identity, old_device)`) as part of the recovery sequence, in the right order
(discharge first, then add the fresh device), so the channel un-seals. This is C3's
`discharge_drift` composed with C6/C7 — a genuine gap, not a scope choice. The
`DeviceRevoked`-broadcast → revocation-aware-resolver path (S1/C1) also has no end-to-end
harness test yet; add one.

### F3 (sub-5a, found in the whole-branch review) — the auto-add path offered REVOKED devices to `add_member`

The 4b auto-add trigger (`add_current_members_to_group`,
`client/src-tauri/src/commands.rs`) built a member's device roster from every
`DeviceAuthorized` in the `FetchDeviceCerts` stream. S1 widened that stream with
`DeviceRevoked`, but the roster only learned to *skip* those payloads — it never
learned to *subtract* the devices they name. So a revoked-but-once-authorized
device stayed on the roster; the fold refuses its add ("declared add of a device
that is not live", `event_log_state.rs:1114-1119`), and because `add_member`
merges locally BEFORE it submits (F1's class), the steward's group was left one
epoch ahead of the server — diverged.

Reachable by this sub-project's own primitives, and by the OWNER (the frontend
gates the trigger on `getOwnPk() === ownerPublicKey`): `reprovision_device`
revokes the old device and publishes a KeyPackage for the fresh one, and a
KeyPackage publish is itself an auto-add trigger, so the old device's surviving
KeyPackages (`fetch_key_packages` has no liveness filter, deliberately — the
server stays blind) were handed to `add_member` on the very next run. The
per-device loop then continued onto the next device with an already-diverged
group.

**Fixed in `d31dd3e`**, in two halves:

1. The roster calls C5's `member_live_leaves` — already the crate's single
   definition of live (authorized + un-revoked + cert-unexpired), mirroring the
   fold's own three checks — instead of hand-rolling the enumeration.
2. The failure CLASS is now classified, not just this instance. F1 already
   established that any rejected own-commit diverges, yet only `stale-epoch` and
   the commit-rate rule were surfaced that way; every other fold refusal came
   back as a plain `Transport` error with `is_diverged()` false. New
   `E2eeError::CommitRejectedDiverged { reason, local_epoch }` is returned by all
   four post-merge submit sites (`bootstrap_group`, `add_member`,
   `rekey_channel`, `discharge_drift`) and is covered by `is_diverged()`, so
   `recover_diverged_group` handles it.

Both guards verified by breaking them and watching the pinning test fail. The
call site in `commands.rs` itself is compile-checked only — no test in the Tauri
crate exercises `add_current_members_to_group` (it needs `AppState` + a live
transport); the behavior it now delegates to is pinned in the crate.
