# Relay Phase 3b — Invite Links for Relayed Servers — Design Spec

**Date:** 2026-06-07
**Status:** Approved (design); ready to plan
**Parent design:** `docs/superpowers/specs/2026-06-06-relay-ip-masking-design.md` (Phase 3)
**Depends on:** Phase 3a (client relay connection) — merged. Gap #3 is already closed.

## Problem

After Phase 3a, a relayed server is reachable via a raw link
`farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<invite_token>` that
`connect_server` parses. But there is no user-facing way to *share* that as a
clickable web link that opens the app and joins — the "Create Invite" flow
(`create_invite`, `commands.rs:1496`) builds a `farder.gg/join/<base64>` link that
only handles the **direct** form, and for a relayed server it would mis-encode
(it naively does `"{server_id}/{code}"`, but `server_id` is the whole relay link).
Separately, the app's deep-link handler (`App.tsx`) currently only `console.log`s
the parsed invite — it never actually connects, so invite links don't complete
even for direct servers.

## Goal

A relayed server's "Create Invite" produces a shareable `farder.gg/join/<base64>`
web link that, when clicked, opens the app and joins the server through its relay —
the same one-click UX direct servers should have. **Self-describing, no backend.**

## Decisions (settled)

| Decision | Choice |
|----------|--------|
| Link strategy | **Self-describing static** (no backend). The link encodes the connection info; the existing static page decodes it. |
| Relay encoding | The relay web link encodes the FULL relay deep link (`farder://relay/.../<new_code>`) as base64url. |
| Direct invites | **Unchanged** — keep the existing `base64url("address/code")` encoding. |
| Domain | Keep `farder.gg` (existing; a one-line constant, trivially changeable to `farder.com`). |
| Deep-link handler | Completed in this phase to actually connect (it was a `console.log` stub). Auto-join on link; a confirm dialog is a Phase 4 polish. |

## Architecture

### Encoding format

- **Relayed server invite:** `create_invite` builds the relay deep link with the
  NEW invite code as the token:
  `farder://relay/<relay_addr>/<server_id_hex>/<cert_fp_hex>/<new_code>`, then
  base64url-encodes that whole string → `https://farder.gg/join/<base64>`.
- **Direct server invite:** unchanged — base64url of `"<address>/<code>"`.

The two are distinguishable after decode: a relay payload starts with `farder://`;
a direct payload is `<address>/<code>` (no scheme).

### `create_invite` change (`client/src-tauri/src/commands.rs:1496`)

After receiving `ServerResponse::InviteCreated { code }`, branch on whether the
server is relayed:

- If `crate::connection::parse_relay_target(&server_id)` is `Some(target)` (the
  server's key/address IS the relay link), build
  `deep_link = format!("farder://relay/{}/{}/{}/{}", target.relay_addr,
  hex(target.server_id), hex(target.cert_fp), code)`, set
  `encoded = base64url(deep_link)`, `link = "https://farder.gg/join/{encoded}"`.
- Else: the existing direct behavior (`plain = "{server_id}/{code}"`, encode, etc.).

`InviteResult { code, link, deep_link }` is returned as today (the UI shows `link`).

### Web page change (`website/js/invite.js`)

The page decodes the `/join/<base64>` payload (it already url-safe-decodes +
`atob`s). After decode:

- If the decoded string **starts with `farder://`** → it IS the deep link; use it
  directly (relay, and any future full-deep-link invites).
- Else → legacy `"<address>/<code>"` → build `farder://<address>/<code>` as today.

The existing landing UX (the "Open in Farder" button, the auto-open attempt, the
"download Farder" fallback) is reused. For a relay link the page can't show a
friendly server address (it's opaque); show a generic "You've been invited to a
Farder server" without the address line, or show the relay host. **Padding note:**
the current decoder does `replace(-/+, _//)` + `atob` but does not re-pad; with
`URL_SAFE_NO_PAD` encoding, add `=` padding before `atob` to be safe across
browsers.

### Deep-link handler completion (`client/src/App.tsx`)

The handler currently parses with `farder://([^/]+)/(.+)` and only logs. Complete
it to actually connect, recognizing both forms:

- If `url` starts with `farder://relay/` → it's a relay link: call
  `api.connectServer(url)` (the whole URL is the address; `connect_server` /
  `parse_relay_target` handle it; no separate invite code — the token is in the
  URL), then dispatch `SERVER_ADDED` + activate, reusing the existing join flow
  (mirror what `ConnectDialog.handleJoin` does after a successful connect).
- Else (direct `farder://addr/code`): parse address + code and call
  `api.connectServer(address, code)` then the same dispatch.
- **Identity-gate timing:** the deep link can arrive before the identity is
  unlocked (the gate). Queue the pending invite URL and process it once `unlocked`
  is true (the gate from the identity feature), so a link clicked at launch still
  works.

Auto-join (no confirm) is acceptable for this phase; a "Join this server?" confirm
is a Phase 4 polish.

## File structure

- `client/src-tauri/src/commands.rs` — `create_invite` relay branch (+ a small
  helper to build the relay deep link from a `RelayTarget` + code; could live in
  `connection.rs` next to `parse_relay_target` as `build_relay_link(target, code)`).
- `client/src-tauri/src/connection.rs` — optional `build_relay_link` helper + unit
  test (round-trips with `parse_relay_target`).
- `website/js/invite.js` — decode-then-detect-`farder://` branch.
- `website/invite/index.html` — only if copy needs adjusting for the opaque relay
  case (minimal).
- `client/src/App.tsx` — complete the deep-link handler (connect + queue-until-unlocked).

## Data flow

```
Relayed server "Create Invite":
  create_invite -> server returns code
  -> build farder://relay/<addr>/<server_id>/<fp>/<code>
  -> link = farder.gg/join/<base64url(deep link)>   (shown to user, copyable)

Friend clicks farder.gg/join/<base64>:
  invite.js: atob -> "farder://relay/.../<code>" (starts with farder://) -> open deep link
  OS -> app receives "deep-link" event with the farder://relay/... URL
  App.tsx: starts with farder://relay/ -> connectServer(url) -> joins via relay (3a)
```

## Error handling

- Malformed/garbage `/join/<base64>` → invite.js shows the existing "invalid invite"
  UI.
- `connectServer` failure (relay down, server offline, bad cert) → surface via the
  existing toast/error path the join flow already uses.
- Deep link before unlock → queued, processed after unlock (not dropped).
- A relay link whose `parse_relay_target` fails in `create_invite` → fall back to
  the direct encoding (or error); since the server key for a relayed server IS a
  valid relay link, this shouldn't happen, but guard it.

## Testing

- **Rust (headless):** unit-test the relay-invite link build — given a `RelayTarget`
  + a code, the produced `farder.gg/join/<base64>` link base64url-decodes back to
  exactly `farder://relay/<addr>/<server_id_hex>/<cert_fp_hex>/<code>`, and
  `parse_relay_target` of that deep link yields the original target with the new
  token. Confirm direct `create_invite` output is unchanged (an existing test or a
  new one over the direct branch).
- **Web JS:** a tiny test (or documented manual check) that the decode branch opens
  the `farder://` payload directly and the legacy `address/code` path still builds
  the direct deep link. (Static site — no test runner; a manual verification note
  is acceptable.)
- **Full GUI flow (UNVERIFIED until the Windows run):** clicking a real
  `farder.gg/join/<…>` link → OS opens the app → the app joins the relayed server.
  This needs the OS deep-link handler + a live relay/server and **cannot run in
  WSL** — flag it UNVERIFIED, like the identity gate.

## Out of scope / deferred

- Backend invite directory / short opaque codes — explicitly rejected for this
  phase (self-describing chosen).
- Relay **management** UI (marking a server relayed in the UI, hiding voice on
  relayed servers, a join-confirm dialog) — **Phase 4**.
- Voice over relay — later phase.
- Shorter links via a bundled default-relay fingerprint — a nicety once the hosted
  default relay is deployed.
- Actually deploying/serving `website/` — existing static-host concern, unchanged.
