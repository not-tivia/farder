# Joining from a browser (guest links): design notes (2026-11-15; step 1 built, rest not)

chris asked for this: people should be able to join a server from a web page such as
`farder.xyz/<server name>/t` with a temporary invite, the way Curse Voice temporary invites
worked and the way people use Discord in a browser without installing it.

## What it is

- **A temp invite link** that opens a page in the browser. The person types a name and is in:
  no install, no account. The invite expires (for example after a few hours) and the guest's
  session ends with it.
- **Guests are limited by default:** read and send in the channels the owner allows, no making
  invites, no powers, maybe no uploads. The owner can make a guest a full member later.
- **Later:** a full web version for people who prefer a browser to the app (what Discord's web
  login is), including voice.

## Why it is not a small feature

| Piece | Today | What the browser needs |
|---|---|---|
| Connection | Farder talks QUIC over UDP | Browsers cannot open raw QUIC or UDP. They can use WebSocket (or WebTransport). A small **gateway** has to turn a browser connection into the QUIC connection the server expects. It can live next to the relay. |
| Identity and signing | Rust crates (`farder-crypto`, `farder-protocol`) | Compile them to **WebAssembly** so the browser makes and keeps the guest's own temporary key, so the gateway cannot speak for the guest. |
| Normal channels | Plain text the host can read | Work as they are. A gateway that forwards bytes sees what a relay sees today. |
| Encrypted channels and DMs | OpenMLS in Rust | Possible in WebAssembly but heavy. First version: guests get normal channels only, and the page says so. |
| The app screens | React/TypeScript | Already web technology. The Tauri commands (about 270) are the part that is desktop only; a guest page needs only about 20 of them (connect, history, send, members) written for the browser. |
| Voice | Opus over QUIC datagrams | Needs Opus in WebAssembly and WebTransport or WebRTC. Leave for the second version. |
| The link | `farder.xyz/join/<code>` already exists for app invites | A `/<server name>/t` route and a page that loads the guest client. |

## The security questions to settle first

1. **Guest identity:** a throwaway key made in the browser. Kept in the browser's storage so a
   refresh does not log them out; discarded when the invite expires.
2. **What the temp invite is:** a new kind of invite in the signed history: short expiry, one
   or a few uses, marked as a guest, so the server can limit what the guest may do.
3. **Abuse:** rate limits on guest joins and messages, an owner switch to turn guest links off,
   and the existing ban rules apply to a guest key.
4. **The gateway sees normal channels.** The same is already true of the relay today. It must
   say so on the guest page.

## Order I would build it in

1. Temp-invite type and guest limits in the server (testable with no browser).
2. The gateway (WebSocket to QUIC) with tests.
3. `farder-crypto` and `farder-protocol` to WebAssembly.
4. A browser build of the guest screens (list of channels, chat, member list).
5. The website route and the link format.
6. Voice and a fuller web client, later.

Steps 1 and 2 are valuable on their own: the gateway is also what a future web app needs.

## Build status

**Step 1, server side (built 2026-11-15, unit-tested, one deliberate break caught):**
`CreateGuestInvite` / `SetGuestLinks` / `GetGuestLinks` (appended to the protocol); guest links are OFF
until the owner turns them on; a guest invite always expires (default 24 h, at most 7 days);
a joiner with one is marked a guest (`members.guest_expires_at`), is capped to view/read/send whatever
roles they get, cannot make invites, and cannot reconnect after the link expires. Normal invites are
unchanged.

**Step 2, the gateway (built 2026-11-15, `docs/modules/gateway.md`):** WebSocket to the server's QUIC main
stream, a dumb frame pump with a route table, certificate pin, per-address cap, size and idle limits; tested
against a fake server and against the real server (a guest joins with its own key, with limits).

**Known gaps in step 1:** on a server with a signed history a guest must also become a LOG member
(`MemberJoined` citing the invite), which only the browser client can sign, so the end-to-end guest
join is step 3/4; no owner screen for the switch yet; direct messages, uploads and reactions are not
yet specifically limited for guests beyond the permission cap.
