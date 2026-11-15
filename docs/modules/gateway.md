# Module: browser gateway (`crates/farder-gateway`)

> **File(s):** `crates/farder-gateway/src/lib.rs`, `src/main.rs`, `tests/gateway.rs`
> **Layer:** Server side (next to the relay). Step 2 of `docs/web-guest-join-design.md`.
> **Last reviewed:** 2026-11-15

## Purpose

Browsers cannot open raw QUIC or UDP, so they cannot talk to a Farder server. The gateway lets
them: it accepts a WebSocket and carries it to the server's QUIC main stream and back.

It is a **dumb frame pump** on purpose. One WebSocket binary message is one Farder frame (the
gateway adds and removes the server's 4-byte length prefix). It never decodes, signs or invents a
frame and holds no key, so it cannot speak for a guest: the guest's own key, made in the browser,
signs the login (a wrong signature is refused by the server, tested).

What it can see: normal-channel traffic in the clear, exactly as a relay does today. The guest page
must say so. It carries only the main stream: chat, history, members. No uploads, no voice.

## Running it

```
farder-gateway --listen 127.0.0.1:8480 --route main=127.0.0.1:4435 [--route other=host:port,fp=<sha256 hex of its certificate>]
```

A browser connects to `ws://<gateway>/ws/<route name>`. Put Caddy in front so browsers use
`wss://` (the gateway itself speaks plain WebSocket and should listen on localhost only). A route
with `fp=` pins the server's certificate (SHA-256 of the DER); a route without it does NOT check the
certificate and is for a server on the same machine or LAN only (the server makes a fresh certificate
each start).

## Guardrails (all tested)

| Guardrail | Behaviour |
|---|---|
| Route table | Only servers the host listed with `--route`; an unlisted name is refused (404) |
| Certificate pin | A server whose certificate is not the pinned one never gets a frame through |
| Per-address cap | `--max-per-ip` (default 8) simultaneous connections; a slot frees when one closes |
| Frame size | Browser frames over `max_frame` (1 MB) close the connection |
| Text messages | Not part of the protocol; close the connection |
| Idle | No traffic for 5 minutes closes it |

## Tested against the real server

`tests/gateway.rs` starts the real `farder-server` binary and speaks the real protocol over a
WebSocket: an owner claims with the setup token, turns guest links on, makes a guest link; a guest
with its own key joins with it, can read server info, is refused both kinds of invite, a used-up
link admits nobody, and a wrong signature is refused with its reason. Build the server first
(`cargo build -p farder-server`).

Found by that test and fixed in the server: after refusing a login (bad signature, bad or expired
invite) the server dropped the connection at once and the reason could be lost; it now lets the
refusal arrive first (`flush_refusal` in `connection.rs`), so a guest sees "this guest link has
expired" instead of "connection lost".

## Not done

The browser side (crypto in WebAssembly, the guest screens, the `farder.xyz/<name>/t` route), a
Caddy example, and joining a server that has a signed history (the guest must also become a log
member, which only the browser client can sign).
