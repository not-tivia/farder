# Farder — working agreement for AI assistants

Farder is a privacy-centric, self-hosted communication platform (Rust server +
crates, React/TypeScript + Tauri client). The product promise is privacy and
security: cryptographic identity, E2EE, IP masking via relays, self-hosting.
**That promise is only real if the features are actually wired up and exercised.**

## The verify-before-done rule (REQUIRED)

Do not claim any feature is "done", "working", "fixed", or "shipping" until you
have **run it and observed the result**. Code that compiles and has passing unit
tests is NOT verified — unit tests check each layer in isolation; they do not
prove the layers are wired together or that the feature is reachable in the real
flow.

Before marking work complete:

1. **Exercise the real path**, not a proxy. For a UI feature, that means the
   actual click → Tauri command → server round-trip → result. For a Rust change,
   run the relevant integration/e2e test or a one-off that drives the real path.
2. **Show the evidence** — the command output, the test result, the observed
   behavior. State plainly what you ran and what you saw. If you could not run
   it (e.g. needs the GUI/audio, which WSL lacks), say so explicitly and mark it
   UNVERIFIED rather than implying it works.
3. **For security/privacy features, verify by observation, not by reading code.**
   "The code calls `encrypt()`" is not enough. Capture the bytes that actually
   leave the process and assert they are ciphertext; confirm the server/relay
   never receives plaintext or the real client IP. Trace the *real* send path and
   confirm the security step is on it.

## Known failure mode: the untyped frontend↔backend seam

`invoke("some_command")` in the client is a **plain string**. Nothing checks at
compile time that a matching `#[tauri::command]` exists and is registered in the
`generate_handler!` list in `client/src-tauri/src/main.rs`. A whole feature can
"exist" on both sides yet be dead because the names don't line up or the command
was never registered (this is exactly how voice-channel join shipped broken).

When adding or touching a Tauri command:
- Confirm the `invoke("X")` name, the `#[tauri::command] fn X`, and the entry in
  `generate_handler!` all agree.
- The frontend↔backend command seam can be audited mechanically: every
  `invoke("...")` name must appear in the handler list. Keep it at zero drift.

## Scope of trust

- **Higher trust:** logic that lives entirely inside Rust (crypto in
  `farder-crypto`, relay routing, transport) — compile-checked as a connected
  whole and unit-tested. Gaps are harder to create here.
- **Lower trust / always verify at runtime:** the frontend↔backend seam, and
  whether a security step is *actually invoked* on the real path rather than
  merely defined.

## Build / test commands

- Rust (all): `cargo test --workspace` (workspace root `/home/deez/farder`).
- Client crate only: `cd client/src-tauri && cargo build` / `cargo test voice::`.
- Frontend type-check: `cd client && npx tsc --noEmit` (no JS test runner).
- Run the desktop client: `cd client && npm run tauri dev` (needs a display +
  audio; **cannot run from WSL while WSLg is disabled** — see project memory).

## Documentation discipline

Farder is open source and growing; contributors (and AI agents) navigate it via
`ARCHITECTURE.md` (the one-page map) and the per-module docs under
`docs/modules/` (one doc per file or tightly-coupled group, using
`docs/modules/_TEMPLATE.md`). The goal: a junior dev can understand what a piece
of code does, how to call it, and what it connects to **without reading the
implementation**.

**Docs are treated like tests: drift is not "done".** When you add or change a
public surface, update the matching doc in the **same commit**. Before marking
any feature complete, run this checklist:

- [ ] New `#[tauri::command]`? → `docs/modules/tauri-commands.md` has an entry
      (name, params, return, side effects) and names the matching
      `invoke("...")` in `tauri-bridge.ts`.
- [ ] New Tauri event in `bridge.rs`? → `docs/modules/tauri-bridge.md` lists the
      event name, payload, and the `useServerEvents.ts` listener that consumes it.
- [ ] New public Rust fn in a crate? → the relevant `docs/modules/*.md` has an entry.
- [ ] New React hook / context action? → `frontend-hooks.md` / `frontend-context.md`.
- [ ] New crate, layer, or data-flow path? → `ARCHITECTURE.md` reflects it.

When auditing or onboarding, prefer reading these docs first; if a doc is
missing or stale for an area you touch, writing/fixing it IS part of the task.
