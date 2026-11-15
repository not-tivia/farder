# Module: browser guest page (`crates/farder-web`, `website/guest/`)

> **Layer:** website + WebAssembly. Steps 3-5 of `docs/web-guest-join-design.md`.
> **Last reviewed:** 2026-11-15

## Purpose

A friend opens `https://<domain>/<server name>/t#<code>` and chats in the browser as a guest: no
install, no account. The page talks to the server through the gateway (`docs/modules/gateway.md`).

## `crates/farder-web` (WebAssembly)

The SAME Rust as the app and server (`farder-crypto`, `farder-protocol`), so the page cannot drift from
the wire format and the guest's key is made and kept in the browser. The page deals in JSON text and
bytes only. Build with `scripts/build_guest_web.sh` into `website/guest/pkg` (not committed).

| Item | What it does |
|---|---|
| `GuestKey::new() / from_secret(bytes) / secret()` | A throwaway identity; the page keeps the secret in `localStorage` so a refresh keeps the same guest |
| `answer_challenge(frame, invite)` | Signs the server's Challenge with the guest's own key and returns the Authenticate frame |
| `begin_join(id, log_server_id, now)` / `finish_join(id, log_id, invite_event, now)` | The two signed events that make the guest a member of a server with a signed history (device authorization, then "I joined with this invite"). The device key is derived from the identity key with SHA-256 and a label |
| `profile_request(id, name)` | The signed display-name update |
| `encode_request(id, json)` / `decode_server_frame(bytes)` | Requests as JSON in, frames as JSON out |

## `website/guest/` (the page)

`index.html`, `guest.css`, `guest.js`. Link forms: `/<name>/t#<code>` (the code is after the `#`, so it
is never sent to the web server or logged) and, for testing, `?server=<name>&gateway=<ws url>#<code>`.
Shows what a guest can and cannot do, and that the gateway can see normal-channel messages. Plain text
channels only; messages are put on the page as text, never HTML. Mind the order: until the guest is a
member of the signed history the server shows no channels, so the page asks for server info again after
joining.

## Safety on the server side

A guest is capped to view/read/send, cannot make invites, and through the signed history may only
submit the device authorization, the join and plain messages (`handlers.rs`, `SubmitEvent`, step 1a).
Their session ends with the link. See `docs/modules/server-permissions.md`.

## Verified

`bash scripts/selftest/guest-suite.sh`: a real Edge browser (Windows) against a real server through the
gateway, 9 checks (join screen and privacy note, joining with the link, history, writing, text-not-HTML,
refresh keeps the guest, a second person on the same link with live messages, wrong code refused, no
script errors). The owner in that test is the Rust fixture `crates/farder-gateway/examples/guest_fixture.rs`.

## The owner's side (in the app)

The invite dialog has a "Guest link" section: the owner switches guest links on, picks how long the link
works (1 hour to 7 days) and the web name (what the host called the server on the gateway), and gets
`https://<site>/<web name>/t#<code>`. Commands: `get_guest_links`, `set_guest_links`, `create_guest_invite`
(`docs/modules/tauri-commands.md`). Verified end to end by `bash scripts/selftest/ubuntu-suite.sh guestlink`.

## Not done

Running behind Caddy over real TLS; phones'
browsers (layout is responsive but untested); voice; encrypted channels; an expired link shown nicely
while already connected.
