# Farder

A privacy-centric, self-hosted communication platform.

## What is Farder?

Farder combines the best of Discord (servers, channels, roles), TeamSpeak (self-hosted), Signal (E2EE), and IRC (lightweight, user-controlled) into a modern platform where:

- **Your identity is a cryptographic keypair** -- no accounts, no emails, no phone numbers
- **All traffic routes through relay nodes** -- your IP is never exposed to servers or other users
- **DMs are end-to-end encrypted** -- the server/relay cannot read your messages
- **Everything is self-hostable** -- no dependency on any central authority

## Project Structure

- `crates/farder-crypto` -- Cryptographic primitives (Ed25519 identity, X25519 key exchange, AES-256-GCM)
- `crates/farder-protocol` -- Protocol message types and MessagePack serialization
- `crates/farder-relay` -- Privacy relay node (QUIC proxy that masks IP addresses)
- `crates/farder-notify` -- Notification relay (offline message delivery)
- `crates/farder-node` -- Personal node library (embedded in client, handles DMs)
- `client/` -- React + TypeScript + Tauri desktop/web client

## Building

```bash
# Build all Rust crates
cargo build --workspace

# Run tests
cargo test --workspace

# Build the client (requires Node.js + system GTK/webkit2gtk libs)
cd client && npm install && npm run build
```

## License

AGPL-3.0-or-later
