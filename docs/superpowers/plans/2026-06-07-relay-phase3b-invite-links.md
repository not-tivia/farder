# Relay Phase 3b — Invite Links for Relayed Servers — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A relayed server's "Create Invite" produces a shareable `farder.gg/join/<base64>` web link that opens the app and joins through the relay — self-describing, no backend.

**Architecture:** `create_invite` (client) encodes the full relay deep link (`farder://relay/.../<new_code>`) as base64url for relayed servers (direct unchanged). The static `website/js/invite.js` decodes it and opens whatever `farder://` deep link it finds. A single relay-aware `parseInviteLink` (extracted + shared) and the completed `App.tsx` deep-link handler actually connect on both forms.

**Tech Stack:** Rust (Tauri command), TypeScript/React, vanilla JS (static site).

**Spec:** `docs/superpowers/specs/2026-06-07-relay-phase3b-invite-links-design.md`

**Depends on:** Phase 3a (`parse_relay_target`, `connect_server` relay branch) — merged.

**Verification note (CLAUDE.md):** the Rust link-encoding is unit-tested headlessly. The frontend + static JS are tsc-/manually-checked. The full **click-link → app-opens → joins** flow needs the OS deep-link handler + a live relay and **cannot run in WSL** — it is **UNVERIFIED until the Windows run**, flagged like the identity gate.

---

## File Structure

- `client/src-tauri/src/connection.rs` — `build_relay_link(target, code)` helper + unit test.
- `client/src-tauri/src/commands.rs` — `create_invite` relay branch.
- `client/src/lib/invite.ts` *(new)* — shared, relay-aware `parseInviteLink`.
- `client/src/components/ConnectDialog.tsx`, `client/src/components/AddServerModal.tsx` — import the shared `parseInviteLink` (remove their local copies).
- `client/src/App.tsx` — complete the deep-link handler (connect + queue-until-unlocked).
- `website/js/invite.js` — decode-then-open-`farder://` branch + base64 padding fix.

---

## Task 1: Rust — `create_invite` builds a relay web link

**Files:** `client/src-tauri/src/connection.rs`, `client/src-tauri/src/commands.rs`.

- [ ] **Step 1: Failing test for the helper.** In `connection.rs`'s `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn build_relay_link_roundtrips_with_new_code() {
        let target = RelayTarget {
            relay_addr: "1.2.3.4:4433".parse().unwrap(),
            server_id: vec![0xaa, 0xbb],
            cert_fp: vec![0xcc, 0xdd],
            invite_token: "old".into(),
        };
        let link = build_relay_link(&target, "NEWCODE");
        assert_eq!(link, "farder://relay/1.2.3.4:4433/aabb/ccdd/NEWCODE");
        // Re-parsing yields the same target with the NEW token.
        let back = parse_relay_target(&link).expect("parses");
        assert_eq!(back.relay_addr, target.relay_addr);
        assert_eq!(back.server_id, target.server_id);
        assert_eq!(back.cert_fp, target.cert_fp);
        assert_eq!(back.invite_token, "NEWCODE");
    }
```

- [ ] **Step 2: Run to verify fail:** `cd ~/farder/client/src-tauri && cargo test build_relay_link_roundtrips 2>&1 | tail -10` — expect a compile error (`build_relay_link` missing).

- [ ] **Step 3: Implement the helper.** Add to `connection.rs` (near `parse_relay_target`):

```rust
/// Build a relay deep link from a target and a (new) invite code:
/// `farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<code>`.
pub fn build_relay_link(target: &RelayTarget, code: &str) -> String {
    format!(
        "farder://relay/{}/{}/{}/{}",
        target.relay_addr,
        hex::encode(&target.server_id),
        hex::encode(&target.cert_fp),
        code
    )
}
```

- [ ] **Step 4: Branch `create_invite`.** In `commands.rs`, `create_invite` (around line 1496), inside the `ServerResponse::InviteCreated { code } =>` arm, replace the direct-only link building with a branch. The current arm is:

```rust
        ServerResponse::InviteCreated { code } => {
            use base64::Engine;
            let plain = format!("{}/{}", server_id, code);
            let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(plain.as_bytes());
            let link = format!("https://farder.gg/join/{}", encoded);
            let deep_link = format!("farder://{}/{}", server_id, code);
            Ok(InviteResult { code, link, deep_link })
        }
```

Replace with:

```rust
        ServerResponse::InviteCreated { code } => {
            use base64::Engine;
            let (encoded, deep_link) =
                if let Some(target) = crate::connection::parse_relay_target(&server_id) {
                    // Relayed server: encode the full relay deep link with the new code.
                    let deep_link = crate::connection::build_relay_link(&target, &code);
                    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(deep_link.as_bytes());
                    (encoded, deep_link)
                } else {
                    // Direct server: existing "address/code" encoding (unchanged).
                    let plain = format!("{}/{}", server_id, code);
                    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(plain.as_bytes());
                    let deep_link = format!("farder://{}/{}", server_id, code);
                    (encoded, deep_link)
                };
            let link = format!("https://farder.gg/join/{}", encoded);
            Ok(InviteResult { code, link, deep_link })
        }
```

- [ ] **Step 5: Run tests + build:** `cd ~/farder/client/src-tauri && cargo test build_relay_link 2>&1 | tail -6 && cargo build 2>&1 | tail -4` — helper test passes, crate builds.

- [ ] **Step 6: Commit:**
```bash
cd ~/farder && git add client/src-tauri/src/connection.rs client/src-tauri/src/commands.rs && \
git commit -m "client: create_invite builds a relay web link for relayed servers"
```

---

## Task 2: Shared, relay-aware `parseInviteLink`

**Files:** Create `client/src/lib/invite.ts`; modify `client/src/components/ConnectDialog.tsx`, `client/src/components/AddServerModal.tsx`.

Both components have a local `parseInviteLink` that handles `farder.gg/join/<base64>` and `farder://addr/code` but NOT relay forms. Extract a single relay-aware version.

- [ ] **Step 1: Create the shared helper.** Create `client/src/lib/invite.ts`:

```ts
export interface ParsedInvite {
  address?: string;
  inviteCode?: string;
  setupToken?: string;
}

// Decode URL-safe base64 (no-pad) used by farder.gg/join links.
function b64urlDecode(s: string): string {
  let t = s.replace(/-/g, "+").replace(/_/g, "/");
  while (t.length % 4) t += "=";
  return atob(t);
}

/**
 * Parse a Farder invite (a pasted link or a farder:// deep link) into a
 * connection target. Relay-aware: a relay deep link (farder://relay/...) is
 * returned whole as `address` (connect_server parses it; the invite token is
 * embedded). Direct links return `address` + `inviteCode`/`setupToken`.
 */
export function parseInviteLink(input: string): ParsedInvite {
  const trimmed = input.trim();
  if (!trimmed) return {};

  // farder.gg/join/ENCODED
  const joinMatch = trimmed.match(/(?:https?:\/\/)?farder\.gg\/join\/([A-Za-z0-9_-]+)/);
  if (joinMatch) {
    try {
      const decoded = b64urlDecode(joinMatch[1]);
      // A full deep link (relay or future direct) is returned whole.
      if (decoded.startsWith("farder://")) {
        return parseInviteLink(decoded);
      }
      // Legacy "address/code".
      const slashIdx = decoded.indexOf("/");
      if (slashIdx > 0) {
        const address = decoded.substring(0, slashIdx);
        const token = decoded.substring(slashIdx + 1);
        if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
        return { address, inviteCode: token };
      }
    } catch {}
    return {};
  }

  // Relay deep link: return the whole URL as the address (token embedded).
  if (/^farder:\/\/relay\//i.test(trimmed)) {
    return { address: trimmed };
  }

  // Direct farder://addr/code
  const farderMatch = trimmed.match(/^farder:\/\/([^/]+)\/(.+)$/i);
  if (farderMatch) {
    const address = farderMatch[1];
    const token = farderMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  // host:port/code (no scheme)
  const slashMatch = trimmed.match(/^([^/]+:\d+)\/(.+)$/);
  if (slashMatch) {
    const address = slashMatch[1];
    const token = slashMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  return {};
}
```

(This consolidates and replaces the two component-local copies. If the existing copies handle a case not above — re-read both and fold it in. The key additions: the `farder://relay/` whole-URL case, the decoded-`farder://` recursion for join links, and the padding fix in `b64urlDecode`.)

- [ ] **Step 2: Use it in `ConnectDialog.tsx`.** Remove the local `function parseInviteLink(...)` (around line 22) and add `import { parseInviteLink } from "../lib/invite";` near the top. Confirm the call sites (`parseInviteLink(inviteInput)`) still type-check against `ParsedInvite`.

- [ ] **Step 3: Use it in `AddServerModal.tsx`.** Same: remove the local `parseInviteLink` (around line 15), import from `../lib/invite`.

- [ ] **Step 4: Type-check:** `cd ~/farder/client && npx tsc --noEmit` — no errors.

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add client/src/lib/invite.ts client/src/components/ConnectDialog.tsx client/src/components/AddServerModal.tsx && \
git commit -m "client: shared relay-aware parseInviteLink"
```

---

## Task 3: Complete the `App.tsx` deep-link handler (connect + queue)

**Files:** `client/src/App.tsx`.

Today the `deep-link` listener only `console.log`s. Complete it to actually join, handling relay + direct, and queue a link that arrives before the identity is unlocked.

- [ ] **Step 1: Add imports + pending state.** In `client/src/App.tsx`, add `import { parseInviteLink } from "./lib/invite";`. In `AppInner`, add state next to `unlocked`:

```tsx
  const [pendingInvite, setPendingInvite] = useState<string | null>(null);
```

- [ ] **Step 2: Capture the deep link (don't act yet).** Replace the body of the `deep-link` listener (the `match`/`console.log` block) with:

```tsx
    const unlisten = listen<string>("deep-link", (e) => {
      setPendingInvite(e.payload);
    });
    return () => { unlisten.then((u) => u()); };
```

- [ ] **Step 3: Process the pending invite once unlocked.** Add a new effect in `AppInner` (after the init effect):

```tsx
  useEffect(() => {
    if (!unlocked || !pendingInvite) return;
    const url = pendingInvite;
    setPendingInvite(null);
    (async () => {
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
    })();
  }, [unlocked, pendingInvite]);
```

(This mirrors `ConnectDialog.handleJoin`'s post-connect dispatch sequence. `api`/`dispatch`/`unlocked` are already in scope in `AppInner`.)

- [ ] **Step 4: Type-check:** `cd ~/farder/client && npx tsc --noEmit` — no errors.

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add client/src/App.tsx && \
git commit -m "client: complete deep-link handler to join (relay+direct), queued until unlocked"
```

---

## Task 4: Static web page — open `farder://` payloads

**Files:** `website/js/invite.js` (and `website/invite/index.html` only if copy needs it).

The page must, after decoding `/join/<base64>`, open a decoded `farder://...` deep link directly (relay), keeping the legacy `address/code` path for direct.

- [ ] **Step 1: Read `website/js/invite.js` fully.** Note `parseInviteUrl()` (decodes `/join/<base64>` to `{server, code}`) and `buildDeepLink(server, code)` (returns `farder://server/code`).

- [ ] **Step 2: Make the decode relay-aware.** In `parseInviteUrl()`, where it `atob`s the `/join/` token: first re-pad the base64 (the encoder uses no-pad URL-safe). Then, if the decoded string starts with `farder://`, return it as a ready-made deep link (use a distinct shape, e.g. `{ deepLink: decoded }`); else the existing `{ server, code }`. Concretely, update the join branch:

```js
    var joinMatch = path.match(/\/join\/([A-Za-z0-9_-]+)/);
    if (joinMatch) {
      try {
        var b64 = joinMatch[1].replace(/-/g, '+').replace(/_/g, '/');
        while (b64.length % 4) b64 += '=';
        var decoded = atob(b64);
        if (decoded.indexOf('farder://') === 0) {
          return { deepLink: decoded };           // relay (or full) deep link
        }
        var slashIdx = decoded.indexOf('/');
        if (slashIdx > 0) {
          return { server: decoded.substring(0, slashIdx), code: decoded.substring(slashIdx + 1) };
        }
      } catch(e) {}
    }
```

- [ ] **Step 3: Use the ready-made deep link when present.** Where the entry point builds the deep link (in `DOMContentLoaded` / `renderValidInvite`), if `parsed.deepLink` is set, use it directly instead of `buildDeepLink(server, code)`. Update the entry point:

```js
    var parsed = parseInviteUrl();
    if (parsed && parsed.deepLink) {
      renderValidInvite(container, null, null, parsed.deepLink);
    } else if (parsed && parsed.server && parsed.code) {
      renderValidInvite(container, parsed.server, parsed.code, null);
    } else {
      renderInvalidInvite(container);
    }
```

And update `renderValidInvite(container, server, code, deepLink)` to take an optional pre-built `deepLink`: `var link = deepLink || buildDeepLink(server, code);` and, when `server` is null (relay), render a generic heading ("You've been invited to a Farder server!") without the server-address line.

- [ ] **Step 4: Manual verification (no JS test runner).** Confirm by reasoning + a node one-liner that a base64url-no-pad of `"farder://relay/1.2.3.4:4433/aabb/ccdd/CODE"` decodes (after re-padding) back to that string and is detected by the `indexOf('farder://') === 0` check. Run:

```bash
cd ~/farder && node -e '
const s="farder://relay/1.2.3.4:4433/aabb/ccdd/CODE";
const b=Buffer.from(s).toString("base64").replace(/\+/g,"-").replace(/\//g,"_").replace(/=+$/,"");
let p=b.replace(/-/g,"+").replace(/_/g,"/"); while(p.length%4)p+="=";
const d=Buffer.from(p,"base64").toString();
console.log("roundtrip ok:", d===s, "| isDeepLink:", d.indexOf("farder://")===0);
'
```
Expected: `roundtrip ok: true | isDeepLink: true`.

- [ ] **Step 5: Commit:**
```bash
cd ~/farder && git add website/js/invite.js website/invite/index.html && \
git commit -m "web: invite page opens relay deep links (self-describing join)"
```

---

## Final verification

- [ ] **Rust + tsc green:** `cd ~/farder/client/src-tauri && cargo test build_relay_link 2>&1 | tail -5 && cargo build 2>&1 | tail -3` and `cd ~/farder/client && npx tsc --noEmit`.
- [ ] **Workspace untouched:** `cd ~/farder && cargo test --workspace 2>&1 | tail -6` — no regressions (this phase doesn't change server/relay/protocol).
- [ ] **Mark UNVERIFIED + ask for the Windows run:** the full flow — create a relay invite in the app → copy the `farder.gg/join/...` link → open it (OS deep link) → app joins the relayed server — is UNVERIFIED in WSL. State it plainly and ask the user to confirm on the Windows build (create invite on a relayed server, click the link, confirm it joins via the relay).
- [ ] **Docs:** mark Phase 3b done in `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md`; extend `docs/modules/client-relay.md` with the invite-link encoding + the deep-link handler.
- [ ] **Finish the branch:** use superpowers:finishing-a-development-branch.

## Notes for the implementer
- Direct invites must be byte-for-byte unchanged (same encoding, same deep link). Only the relay branch is new.
- `parseInviteLink` is now shared — make sure BOTH former consumers (ConnectDialog, AddServerModal) use the shared version and still behave for direct links.
- The deep-link handler auto-joins (no confirm dialog) — that's intended for this phase; a confirm is Phase 4.
- The GUI/deep-link/web flow can't run in WSL; the Rust encoding round-trip + tsc are the headless guards.
