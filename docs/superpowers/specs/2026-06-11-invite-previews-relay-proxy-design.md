# Invite Previews via Relay Fetch Proxy

**Date:** 2026-06-11
**Status:** Approved by owner (design conversation 2026-06-11, evening session)
**Approach:** A — unified relay proxy (owner picked over hybrid and static-link previews)

## Problem

In-chat invite cards (`InviteEmbed.tsx`, shipped 2026-06-11 morning) show only a
RELAYED/DIRECT badge and a Join button — no server name or counts, because no
data source exists: every server request requires authentication, and a viewer
fetching a preview from a DIRECT server would leak their IP merely by seeing
the message. The owner's architecture decision: the RELAY becomes the privacy
fetch proxy. This spec covers phase one — invite previews. Rich external embeds
(YouTube/fxtwitter) are the NEXT feature on the same foundation and are OUT of
scope here.

## Decisions (owner)

- **Slice:** invite previews first; external embeds as the immediate follow-up
  feature reusing the proxy foundation.
- **Auto-preview BOTH relayed and direct invites.** The earlier click-to-preview
  rule for direct invites existed because the viewer's client would have dialed
  the server itself; with the relay proxying, that leak is gone, so the rule is
  retired.
- **Approach A:** one proxy mechanism for both server kinds; the viewer's
  machine only ever talks to a relay.

## Architecture

### New server question: `GetInvitePreview` (pre-auth, code-gated)

A new `ClientFrame::GetInvitePreview { code: String }` variant, accepted
INSTEAD of `Authenticate` on a fresh primary stream (no challenge handshake).
The server validates the code via the existing `invites::validate_invite`
(exists, not expired, uses remaining):

- Valid → respond `ServerFrame::InvitePreview { server_name: String,
  member_count: u32, online_count: u32 }` and close the stream.
  `member_count` = `members::list_members().len()` (active members);
  `online_count` = currently connected clients (`state.clients` map size).
- Invalid/expired/exhausted → respond
  `ServerFrame::InvitePreviewError { reason: "invalid" }` and close. The reason
  string is deliberately uniform — invalid codes reveal NOTHING about the
  server (no name, no counts, no exists-vs-expired distinction), so codes
  cannot be used to enumerate or probe servers.

No authentication, no member registration, no session token. The connection is
throwaway and closed by the server after one answer.

### New relay request: `ProxyInvitePreview`

New `Message` variants (crates/farder-protocol/src/messages.rs):

- `ProxyInvitePreview { target: PreviewTarget, code: String }` where
  `PreviewTarget::Registered { server_id: Vec<u8> }` (relayed server) or
  `PreviewTarget::Direct { addr: String }` (host:port).
- `ProxyInvitePreviewResult { result: PreviewOutcome }` with
  `enum PreviewOutcome { Preview { server_name: String, member_count: u32,
  online_count: u32 }, Invalid, Unavailable }` — `Invalid` mirrors the server's
  uniform invalid-code answer; `Unavailable` covers timeout, dial failure,
  SSRF refusal, rate-limit refusal, and undecodable answers.

Relay handling:

- **Registered target:** the relay opens a new bridged stream on the server's
  existing registration connection — stamped with the RESERVED handle 0
  (relay-originated; clients can never forge it since `bridge_client` stamps
  real client handles authoritatively and handle 0 is rejected at auth) — sends
  `RelayStreamRole::Primary` then `GetInvitePreview`, reads one answer.
- **Direct target:** the relay dials the address with a QUIC client endpoint
  using the same permissive certificate acceptance the Farder client uses for
  direct servers, sends `GetInvitePreview` on the primary stream, reads one
  answer, closes.
- Either way the relay forwards the answer back verbatim as
  `ProxyInvitePreviewResult`; it does not parse beyond routing needs.

### Which relay does the asking

- Relayed invite links name their relay (`farder://relay/<addr>/...` or
  compact `relayd` = compiled-in default). The preview request goes to THAT
  relay — it is the only one that knows the `server_id`, and this keeps
  self-hosted relays fully functional.
- Direct invite links have no relay; the request goes to the compiled-in
  default Farder relay (`default_relay.rs`).

### Client

- New Tauri command `get_invite_preview(link: String)` →
  `{ status: "ok" | "invalid" | "unavailable", serverName?, memberCount?,
  onlineCount? }`. It parses the link (reusing `parse_relay_target` /
  the direct-form parsing), opens a THROWAWAY connection to the chosen relay
  (pinned cert for relay targets; default relay pinned fp for direct targets),
  sends `ProxyInvitePreview`, reads the result, closes. Never touches session
  connections; lazy only (PIN-lock rule).
- Client-side cache: in-memory per app session keyed by (normalized link,
  code), TTL ~60 s, so re-renders and multiple cards for the same invite don't
  re-ask.
- `InviteEmbed.tsx` v2: loading state → preview (server name, existing
  RELAYED/DIRECT badge, "N members · M online") or "Preview unavailable" or
  "Invite invalid or expired". Join button unchanged (opens the JoinConfirm
  gate).
- `JoinConfirmModal` shows the server name when a preview is available
  ("Join <name>?"); falls back to today's link display otherwise.
- New UI classes styled in ALL THREE theme files per CLAUDE.md.

## Guardrails (relay)

- **Rate limit:** per-IP sliding-window limit on `ProxyInvitePreview` requests
  (reuse `limits.rs` machinery with its own bucket, e.g. 30 previews/min/IP),
  separate from the connection-admission limit.
- **Timeout:** 5 s end-to-end per lookup (dial + question + answer); on expiry
  → `unavailable`.
- **SSRF guard:** `Direct` targets resolving to loopback, private (RFC 1918),
  link-local, or otherwise non-global addresses are REFUSED → `unavailable`.
  The relay must not be usable to probe its own host or anyone's LAN.
- **Size cap:** the preview answer is one small frame; the relay enforces a
  16 KB read cap on the answer (a hostile "server" can't stream garbage).
- **Cache:** in-memory TTL cache (60 s) keyed by (target, code), capped entry
  count (e.g. 1024, LRU): a popular invite viewed by many clients costs one
  upstream lookup per minute. Nothing persisted to disk.

## Privacy notes

- Viewer IPs are hidden from server hosts in ALL cases — the host sees only
  the relay connecting. This is a strict improvement for direct servers.
- The proxying relay sees: requester IP (inherent — it's the proxy), target
  server, and the invite code. Same trust class as what relays already carry;
  no new trust introduced. The E2E-tunnel hardening backlog item is unaffected.
- Invalid codes return a uniform error with zero server information.

## Limits, edge cases, rollout

- **Old servers** (pre-feature builds) fail to decode `GetInvitePreview` and
  drop the throwaway connection → relay times out → card shows "Preview
  unavailable". Because previews never ride session connections, version skew
  here can never cause reconnect loops (disco-ball lesson, 2026-06-11).
- **Old relays** don't understand `ProxyInvitePreview` → same graceful
  `unavailable` path client-side. The deployed default relay needs ONE
  redeploy on the VPS (`git pull` + `docker compose -f
  deploy/relay/docker-compose.yml up -d --build`) — owner-driven, guided, same
  as the rate-limit redeploy.
- Offline direct server → `unavailable` after the 5 s timeout.
- Setup-token links and plain-address links (no invite code) get no preview —
  the card renders as it does today.
- Counts are point-in-time with up to ~2 min of staleness (relay 60 s + client
  60 s caches) — acceptable for a preview.

## Out of scope (this phase)

- Rich external embeds (YouTube/fxtwitter oEmbed/OpenGraph via the relay) —
  next feature, same proxy foundation, needs the domain allowlist + larger
  caching design.
- Server icons in previews (server avatars are still client-local only).
- Invite-only vs open badge (all Farder servers are effectively invite/token
  gated today; nothing to display).
- farder.gg registration / web previews.

## Verification plan

Headless here: relay router tests with real-QUIC doubles (existing pattern in
`router.rs` mod tests) covering registered-target proxying, direct-target
proxying, SSRF refusal, timeout → unavailable, rate-limit refusal, cache hit;
server handler tests for the code gate (valid → preview with correct counts;
expired/exhausted/unknown → uniform invalid; no member row created); client
link-routing unit tests (which relay gets asked for each link form) + Tauri
seam check; `cargo test --workspace`, client crate build, `npx tsc --noEmit`.
Real verification is the owner's: redeploy the relay on the VPS, then on
Windows paste a relayed invite and a direct invite → cards fill with
name/counts; an expired code shows "Invite invalid or expired". UNVERIFIED
until that run per CLAUDE.md.
