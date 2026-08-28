# Mesh Rung 2 — Sub-project 5b: Lifecycle GUI — Implementation Plan

**Goal:** make 5a's lifecycle machinery reachable, and keep E2EE channels alive.

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` sub-project 5
(leaf-change notices, per-identity device counts, device-loss recovery copy).
Baseline: `main` @ `82329bc`, 921 workspace + 191 client tests green.

## What recon changed about this sub-project

5b was scoped as "the GUI half". It is not. **None of 5a's lifecycle is reachable
from the client** — `rekey_channel`, `discharge_drift`, `revoke_device`,
`reset_group`, `join_reset`, `add_own_device`, `recover_diverged_group` and
`should_rekey` have **zero** references in `client/src-tauri`. Two of those gaps
are not missing polish; they BRICK CHANNELS:

1. **The freshness ceiling.** `FRESHNESS_CEILING_EVENTS = 500`
   (`event_log_state.rs:49`): once 500 channel events accumulate with no accepted
   commit, the fold refuses sealed content "until somebody rekeys". Nothing in the
   client rekeys. An E2EE channel with no membership churn therefore stops
   accepting messages after ~500 events, permanently, with no path out of the UI.
2. **The pending-removals seal.** A ban, a `DeviceRevoked` or a cert expiry turns
   that leaf into drift, which SEALS the channel until a remaining member authors
   a remove-commit. Nothing in the client discharges drift. So banning a member
   bricks the channel.

Both are the same shape as 5a's own findings: one half of a mechanism built, the
other half never exercised. The client's send path currently keys on
`is_stale_epoch` only — `is_freshness_ceiling_reached` and
`is_sealed_pending_removals` (both shipped in 5a, both tested) have no caller.

**So 5b leads with keep-alive, not with notices.** The split follows the project's
usual verifiability line:

- **5b-1 — keep-alive + actions (headless, WSL-verifiable).** The triggers and the
  Tauri command layer. Provable in the existing harness against a real server.
- **5b-2 — the GUI (needs the owner's Windows run).** Transparency notices, device
  counts, the five lifecycle states, confirmations, theming ×3.

## Scope decisions (verified against recon)

**D1 — transparency notices are CLIENT-rendered, never server-authored.** The
server cannot write into an E2EE channel by construction (the write choke point,
pinned by `security_observation.rs`'s "no public message door" and "the sweeper
announces nothing into a sealed channel" tests). A notice is therefore derived
locally from the leaf-set diff the steward already computes.

**D2 — the leaf diff comes from `group.leaves()` (the ACTUAL view), not from the
commit's declared adds/removes.** Same posture as 4b's idempotency guard and 4a's
Gate 2: what the tree really holds, not what a commit claimed. A notice built from
declared data would be a notice an attacker writes.

**D3 — notices persist in the sealed history store**, in their own table, sealed
by the same key. A transparency notice you can miss by restarting is not a
transparency notice. Reuses 7a's purge paths.

**D4 — device counts come from the channel's leaf set, not from
`member_live_leaves`.** The security-relevant number is "how many of Alice's
devices can read THIS channel", which the group already knows — and it costs no
extra round trip per member.

**D5 — rekey is driven by 5a's `should_rekey` (pure, loop-free) plus the reactive
ceiling signal.** Cadence state (`RekeyCadence`) persists in the existing
`mls_state.json`. The client never invents a rekey policy of its own.

## Tasks — 5b-1 (keep-alive + actions, headless)

- [x] **K1 — react to the two seal signals on send.** `send_sealed_message` maps
      `is_freshness_ceiling_reached` → rekey-then-retry (once), and
      `is_sealed_pending_removals` → surface a typed, actionable state. No loops:
      one attempt, then a typed error the UI can act on.
- [x] **K2 — the rekey trigger.** Persist `RekeyCadence` in `mls_state.json`;
      consult `should_rekey` on channel open and after each own send; call
      `rekey_channel` when it says so. Proactive cadence AND the ceiling override.
- [x] **K3 — the drift-discharge trigger.** On the sealed-pending-removals signal,
      derive the dead leaves (5a's `dead_leaves_from_revocation` / the ban's
      membership change) and call `discharge_drift`. Handle the race exactly as
      5a specified: one attempt, `stale-epoch` → back off, never spin.
- [x] **K4 — lifecycle commands.** `revoke_own_device`, `revoke_member_device`
      (owner), `reset_e2ee_channel` (owner), `rekey_e2ee_channel` (manual escape
      hatch), each a thin wrapper over the crate, each documented in
      `tauri-commands.md` in the same commit and cross-checked by `seam_audit.py`.
- [x] **K5 — the leaf-diff surface.** Extend the steward's result with the
      `group.leaves()` diff per applied commit (D2), so 5b-2 renders notices from
      a fact the crate computed rather than the frontend guessing.
- [x] **K6 — harness proof (named deliverable).** (both bricking paths proven; rekey cadence K2 still to come) Extend `tests/e2ee_lifecycle.rs`:
      drive a channel PAST the freshness ceiling and prove it keeps accepting
      messages with the trigger in place — and, with the trigger disabled, that it
      seals. Same for a ban → drift → discharge → un-seal. **This is the test that
      would have caught the bricking bug**, so it is the deliverable, not a nicety.

## Tasks — 5b-2 (GUI, needs the Windows run)

- [ ] **G1 — leaf-change notices** rendered in-channel from K5's diff, persisted
      per D3, non-dismissible, with copy that names the person and the device.
- [ ] **G2 — per-identity device count** in the member sidebar (D4).
- [ ] **G3 — the lifecycle states**: "needs a key refresh" (ceiling), "sealed until
      a device is removed" (drift), "re-provisioned — history for that device is
      gone", and the two 5a carry-forwards that MUST be said out loud: a
      single-device identity cannot self-recover a lost store (owner reset is the
      only escape), and recovery costs a fresh device key.
- [ ] **G4 — the actions**: retire this device, revoke a member's device (owner),
      reset the channel (owner) — each behind a confirmation that states what is
      lost, because each is irreversible.
- [ ] **G5 — theming ×3.** Every new class gets CSS in `discord-dark`,
      `hello-kitty` and `xp-luna-blue`, driven by `var(--xp-…)`, never hard-coded
      colors. A `className` with no CSS renders raw — that is a bug, not a detail.

## Gates
- `cargo test --workspace` ≥ 921, never fewer; client crate tests ≥ 191.
- `cargo clippy` clean on touched crates; `cd client && npx tsc --noEmit`.
- `python3 scripts/seam_audit.py` passes.
- `git ls-files --eol` after scripted edits (the CRLF trap).
- `grep -l "<new-class>" client/src/themes/*/theme.css` lists all three themes.

## Review discipline
Break every load-bearing guard and watch its test fail. Load-bearing here: the
ceiling trigger (K1/K2 — the anti-bricking property), the drift race (K3, which
must not spin), the leaf diff's use of the ACTUAL view (K5), and the
irreversibility confirmations (G4).

## Carry-forwards (recorded, not done)
- Sub-6 (E2EE attachments) and the rest of sub-7 (export/import) stay out of scope.
- The 4-digit-PIN ceiling ([[at-rest keys]]) is a product decision, not 5b's.
