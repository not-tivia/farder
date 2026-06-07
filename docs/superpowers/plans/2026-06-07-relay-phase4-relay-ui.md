# Relay Phase 4 — Relay UX Polish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Polish the relay *join* experience — disable voice on relayed servers in the UI, and add a "Join this server?" confirmation before a deep-link invite connects.

**Architecture:** Surface the backend `relayed` flag through `ConnectResult` → frontend `PerServerState`. `ChannelSidebar` disables voice-channel joins (toast + greyed) when the active server is relayed. `App.tsx` routes a pending deep-link invite through a small confirm modal instead of auto-joining.

**Tech Stack:** Rust (Tauri command), TypeScript/React.

**Spec:** `docs/superpowers/specs/2026-06-07-relay-phase4-relay-ui-design.md`

**Depends on:** Phases 1–3b (merged). Piece 1 (create a relayed server in-app) is OUT OF SCOPE (deferred until the default relay is deployed).

**Verification note (CLAUDE.md):** Rust is build-gated; frontend is `tsc`-gated. The voice-disabled styling/toast and the confirm modal are visual/runtime behavior that **cannot render in WSL** — **UNVERIFIED until the Windows run**, flagged like prior phases.

---

## File Structure

- `client/src-tauri/src/commands.rs` — `ConnectResult.relayed` + set it in `connect_server`; `relayed: false` in `create_local_server`'s payload.
- `client/src/lib/types.ts` — `ConnectResult.relayed?: boolean`.
- `client/src/context/ServerContext.tsx` — `PerServerState.relayed` + `initialPerServerState` default + `SERVER_ADDED` reducer.
- `client/src/components/ChannelSidebar.tsx` — voice-disable on relayed.
- `client/src/components/JoinConfirmModal.tsx` *(new)* — the confirm modal.
- `client/src/App.tsx` — route the pending invite through the modal.

---

## Task 1: Rust — surface `relayed` on `ConnectResult`

**Files:** `client/src-tauri/src/commands.rs`.

- [ ] **Step 1: Add the field.** In `ConnectResult` (`commands.rs:44`), add `pub relayed: bool`:

```rust
pub struct ConnectResult {
    pub server_name: String,
    pub member_count: u32,
    pub channels: Vec<ChannelInfo>,
    pub categories: Vec<CategoryInfo>,
    pub roles: Vec<RoleInfo>,
    pub owner_public_key: Option<farder_crypto::identity::PublicKey>,
    pub relayed: bool,
}
```

- [ ] **Step 2: Set it in `connect_server`.** In `connect_server`, the connect branch (Phase 3a) binds a `relayed` boolean (the tuple `(endpoint, conn, send, recv, session_token, relayed)`). Find where `connect_server` builds and returns its `ConnectResult { ... }` (it constructs it from the initial server info after connecting) and add `relayed,` to that struct literal. (The `relayed` variable is in scope from the connect branch.)

- [ ] **Step 3: Set it in `create_local_server`'s payload.** `create_local_server` returns a `serde_json::Value` connect-payload. Find where it builds that JSON object (it includes `server_name`, `channels`, etc.) and add `"relayed": false` to it (a local server is never relayed). If it builds the payload by serializing a `ConnectResult`, then Step 1 already covers it — just ensure the `ConnectResult` it builds sets `relayed: false`.

- [ ] **Step 4: Build.** `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -8` — must build. The compiler will flag every `ConnectResult { ... }` construction missing the new field — fix each (relay branch → the `relayed` var; all other/direct/local constructions → `false`). Run `cd ~/farder && grep -rn "ConnectResult {" client/src-tauri/src` to be sure none are missed.

- [ ] **Step 5: Confirm the existing suite still builds/passes:** `cd ~/farder/client/src-tauri && cargo test 2>&1 | tail -5` (serial-flaky tests aside — `book`/`display` fps are known flakes; run `-- --test-threads=1` if needed).

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/commands.rs && \
git commit -m "client: surface relayed flag on ConnectResult"
```

---

## Task 2: Thread `relayed` into frontend `PerServerState`

**Files:** `client/src/lib/types.ts`, `client/src/context/ServerContext.tsx`.

- [ ] **Step 1: TS type.** In `client/src/lib/types.ts`, find the `ConnectResult` interface and add `relayed?: boolean;` (optional for backward-compatible payloads):

```ts
  relayed?: boolean;
```

- [ ] **Step 2: `PerServerState` field.** In `client/src/context/ServerContext.tsx`, add to `PerServerState` (after `ownerPublicKey`):

```ts
  relayed: boolean;
```

- [ ] **Step 3: Default in `initialPerServerState`.** Find `initialPerServerState` (the object used as `{ ...initialPerServerState }`) and add `relayed: false,` so every per-server state has a default.

- [ ] **Step 4: Copy it in `SERVER_ADDED`.** In the `SERVER_ADDED` reducer case, in the `newPerServer` object, add:

```ts
        relayed: payload.relayed ?? false,
```

- [ ] **Step 5: Type-check.** `cd ~/farder/client && npx tsc --noEmit` — no errors. (If `payload`'s type doesn't include `relayed`, the optional field on `ConnectResult` from Step 1 + `?? false` covers it.)

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add client/src/lib/types.ts client/src/context/ServerContext.tsx && \
git commit -m "client: thread relayed flag into PerServerState"
```

---

## Task 3: Disable voice on relayed servers (`ChannelSidebar`)

**Files:** `client/src/components/ChannelSidebar.tsx`.

The voice-channel row's `onClick` (around `:367`) calls `api.joinVoice`. When the active server is relayed, refuse with a toast and grey the row. `activeServer` is the active `PerServerState` (used at `:360` as `activeServer?.voiceStates`); `toast` is imported (used at `:385`).

- [ ] **Step 1: Guard the join click.** In the voice-channel `onClick` handler, add a relayed guard as the FIRST thing inside (before the `if (!serverId) return;` or right after it):

```tsx
          onClick={async () => {
            if (!serverId) return;
            if (activeServer?.relayed) {
              toast.error("Voice isn't available over a relay yet");
              return;
            }
            if (isInThisChannel) {
              await leaveVoiceChannel(ch.id);
            } else {
              // ... existing join logic unchanged ...
```

- [ ] **Step 2: Grey the row when relayed.** On the voice-channel `<div>` (the one with `className={`channel-item voice-channel...`}`), add a dimmed style when relayed. Add to its `style` (or merge with any existing style):

```tsx
          style={{ opacity: activeServer?.relayed ? 0.5 : undefined, cursor: activeServer?.relayed ? "not-allowed" : undefined }}
          title={activeServer?.relayed ? "Voice isn't available over a relay yet" : undefined}
```

(If the div already has a `style` prop, merge these keys in; if not, add the `style`/`title` props. Keep the existing `className`/`data-drag-*`/`onClick` props.)

- [ ] **Step 3: Type-check + confirm `activeServer` is the relayed-bearing state.** `cd ~/farder/client && npx tsc --noEmit` — no errors. Confirm `activeServer` resolves to the active server's `PerServerState` (it's already used for `voiceStates`); `activeServer?.relayed` is now a valid field (from Task 2). If `activeServer` is a `ServerListEntry` (not `PerServerState`) at this spot, instead read the relayed flag from the per-server state the component already uses for `voiceStates` — use the SAME object that `activeServer?.voiceStates[ch.id]` reads from.

- [ ] **Step 4: Commit:**
```bash
cd ~/farder && git add client/src/components/ChannelSidebar.tsx && \
git commit -m "client: disable voice join on relayed servers (toast + greyed)"
```

---

## Task 4: "Join this server?" confirm modal (`App.tsx` + `JoinConfirmModal`)

**Files:** Create `client/src/components/JoinConfirmModal.tsx`; modify `client/src/App.tsx`.

Today the pending-invite effect (Phase 3b) connects immediately. Route it through a confirm modal.

- [ ] **Step 1: Create the modal.** Create `client/src/components/JoinConfirmModal.tsx`:

```tsx
export default function JoinConfirmModal({
  onConfirm,
  onCancel,
}: {
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Join server</span>
          <button className="modal-close" onClick={onCancel}>X</button>
        </div>
        <div className="modal-body">
          <p>You've been invited to a Farder server. Join it?</p>
          <div className="connect-actions">
            <button className="xp-button" onClick={onConfirm}>Join</button>
            <button className="xp-button" onClick={onCancel}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
```

(These classes — `modal-overlay`, `modal-dialog`, `modal-titlebar`, `modal-close`, `modal-body`, `connect-actions`, `xp-button` — are the ones `InviteDialog.tsx` uses; reuse them for consistency.)

- [ ] **Step 2: Import + state in `App.tsx`.** Add `import JoinConfirmModal from "./components/JoinConfirmModal";`. In `AppInner`, add state next to `pendingInvite`:

```tsx
  const [joinConfirm, setJoinConfirm] = useState<string | null>(null);
```

- [ ] **Step 3: Extract the join logic into a function.** Add a `joinFromInvite` function in `AppInner` (the async connect+dispatch body that currently lives inside the pending-invite effect):

```tsx
  async function joinFromInvite(url: string) {
    const parsed = parseInviteLink(url);
    if (!parsed.address) {
      console.error("[deep-link] unrecognized invite:", url);
      return;
    }
    try {
      const result = await api.connectServer(parsed.address, parsed.inviteCode, parsed.setupToken);
      dispatch({ type: "SERVER_ADDED", serverId: parsed.address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: parsed.address });
      try {
        const members = await api.getMembers(parsed.address);
        dispatch({ type: "SET_MEMBERS", serverId: parsed.address, payload: members });
      } catch {}
      try {
        const dms = await api.listDms(parsed.address);
        dispatch({ type: "SET_DMS", serverId: parsed.address, payload: dms });
      } catch {}
    } catch (e) {
      console.error("[deep-link] failed to join from invite:", e);
    }
  }
```

- [ ] **Step 4: Route the pending invite to the modal (don't auto-join).** Replace the body of the pending-invite effect so it opens the confirm instead of connecting:

```tsx
  useEffect(() => {
    if (!unlocked || !pendingInvite) return;
    setJoinConfirm(pendingInvite);
    setPendingInvite(null);
  }, [unlocked, pendingInvite]);
```

- [ ] **Step 5: Render the modal.** In `AppInner`'s main-app return (the branch rendered when `unlocked` and not `initializing` — alongside the main content / near `<ToastContainer />`), render the modal when `joinConfirm` is set:

```tsx
      {joinConfirm && (
        <JoinConfirmModal
          onConfirm={() => { const u = joinConfirm; setJoinConfirm(null); void joinFromInvite(u); }}
          onCancel={() => setJoinConfirm(null)}
        />
      )}
```

(Place it where other top-level overlays/`ToastContainer` render so it overlays the app. If the app's main return is a fragment with `<AppShell/>` + `<ToastContainer/>`, add it as a sibling there.)

- [ ] **Step 6: Type-check.** `cd ~/farder/client && npx tsc --noEmit` — no errors.

- [ ] **Step 7: Commit:**
```bash
cd ~/farder && git add client/src/components/JoinConfirmModal.tsx client/src/App.tsx && \
git commit -m "client: confirm before joining a server from a deep-link invite"
```

---

## Final verification

- [ ] **Rust builds + frontend type-checks:** `cd ~/farder/client/src-tauri && cargo build 2>&1 | tail -3` and `cd ~/farder/client && npx tsc --noEmit`.
- [ ] **Workspace untouched:** `cd ~/farder && cargo test --workspace 2>&1 | tail -6` — no regressions (this phase only touches the client crate's `ConnectResult`).
- [ ] **Mark UNVERIFIED + Windows ask:** the visual behavior — voice channels greyed + the toast on a relayed server, and the "Join this server?" modal appearing when a `farder.gg/join/...` link is clicked — is UNVERIFIED in WSL. State it plainly and ask the user to confirm on the Windows build (join a relayed server via an invite, see the confirm modal, then confirm voice is greyed/toasts on that server).
- [ ] **Docs:** mark Phase 4 (pieces 2+3) done in `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`; note in `docs/modules/client-relay.md` that voice is UI-disabled on relayed servers and invites are confirm-gated.
- [ ] **Finish the branch:** use superpowers:finishing-a-development-branch.

## Notes for the implementer
- Piece 1 (create a relayed server in-app) is OUT OF SCOPE — do not touch the local-server spawn/relay-mode wiring.
- Direct servers and existing voice behavior on non-relayed servers must be unchanged — the relayed guard only triggers when `activeServer?.relayed` is true.
- The confirm modal must apply to BOTH relay and direct invite links (uniform) — `joinFromInvite` handles both via `parseInviteLink`.
- GUI behavior can't run in WSL; `tsc` + `cargo build` are the headless guards.
