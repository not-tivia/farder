# Privacy-Centric Self-Hosted Communication Platform — Design Spec

**Date:** 2026-04-01
**Status:** Draft
**Name:** Farder
**License:** AGPLv3

## Vision

A self-hosted communication platform combining the best of Discord (servers, channels, roles, rich chat), TeamSpeak (self-hosted, community-owned), Signal (E2EE, cryptographic identity), and IRC/MSN (lightweight, user-controlled). The core differentiator is a privacy-first architecture where user IPs are never exposed, identity is cryptographic and portable, and the entire stack is self-hostable with no dependency on any central authority.

## Architecture

### Overview

Three layers compose the system:

1. **Client Application** — React + TypeScript, packaged with Tauri for desktop, served as a web app for browsers. Handles all cryptographic operations locally.
2. **Relay Layer** — Self-hostable proxy network. All traffic (text, voice, media) routes through relay nodes. Neither clients nor servers see each other's real IP addresses.
3. **Server** — A single Rust binary with modular internals. Hosts communities (channels, roles, message history). Self-hosted by anyone.

Supporting services (all optional and self-hostable):
- **Notification Relay** — Persistent push delivery for offline users.
- **Server Directory** — Optional public listing of servers for discoverability.
- **TURN/STUN Relay** — NAT traversal for voice/media connections.

### Technology Choices

| Component | Technology | Rationale |
|---|---|---|
| Server | Rust (modular monolith) | Performance for real-time workloads, memory safety, single-binary deployment |
| Transport | QUIC | Modern UDP-based protocol, built-in encryption, handles packet loss well for voice |
| Client UI | React + TypeScript | Large ecosystem, shared codebase for web and desktop |
| Desktop wrapper | Tauri | Lightweight (~10-20MB), uses system webview, Rust backend aligns with server stack |
| Database | SQLite | Zero external dependencies, embedded, sufficient for single-server workloads |
| File storage | Local filesystem | Simple, no external dependencies |
| Voice codec | Opus | Industry standard, low latency, speech and music |
| Video codec | VP9 / AV1 | Modern, efficient, royalty-free |
| Cryptographic identity | Ed25519 | Fast signatures, small keys, well-audited |
| E2EE | X25519 key exchange + AES-256-GCM | Standard, proven, performant |

### Server Internal Modules

The server is a single binary with six internal modules communicating through well-defined Rust interfaces (traits). Modules can be extracted into separate services later if scale demands it.

1. **Identity & Auth** — Verifies cryptographic signatures, manages public key registration on the server, handles session tokens.
2. **Permissions** — Role-based access control, permission resolution (server → category → channel), server-side filtering of all responses.
3. **Text/Chat** — Channels, categories, DMs relay, message storage, search indexing (non-E2EE channels only).
4. **Voice/SFU** — Selective Forwarding Unit for voice and video. Receives streams, forwards to participants without mixing or transcoding.
5. **E2EE Engine** — Facilitates key exchange between clients for encrypted DMs and private channels. The server never possesses decryption keys.
6. **Storage** — SQLite for metadata, messages, permissions. Filesystem for attachments. Handles message retention/auto-purge.

## Cryptographic Identity

### How It Works

- On first launch, the client generates an **Ed25519 keypair**.
- The **public key** is the user's identity (e.g., `vk_8a3f...b2c1`).
- The **private key** stays on the device, never leaves.
- A user profile (display name, avatar, status) is signed by the private key, preventing impersonation.

### Day-to-Day

- **Joining a server:** Client presents public key + signed profile. Server verifies the signature. No passwords, no account creation forms.
- **Adding a friend:** Exchange public keys via QR code, link, or finding each other on a mutual server.
- **Switching devices:** Export private key encrypted with a passphrase, import on the new device.
- **Losing a key:** That identity is gone. Mitigated by backup options.

### Key Backup Options

- Export to encrypted file (user's responsibility).
- Paper key / mnemonic phrase.
- Encrypted backup to a user-designated server (convenience vs. trust tradeoff).

### Key Revocation

At keypair creation, the user sets a 4-digit recovery PIN. If a private key is compromised:

1. User generates a new keypair on a new (or wiped) device.
2. Presents the old public key + 4-digit PIN.
3. Server verifies the PIN, marks the old key as revoked.
4. A signed revocation notice propagates to all servers that have seen the old key.
5. The old key can no longer authenticate anywhere.

The compromised private key is not needed for revocation — only the PIN.

### What the Server Sees

- Public key and signed profile. Nothing more.
- No email, no phone number, no IP address.
- Server can ban a public key but cannot determine who is behind it.

## Relay / Privacy Layer

### Purpose

The relay layer is the core privacy guarantee. It sits between every client and every server, always. Neither side ever learns the other's IP address.

### How It Works

1. Client connects to a relay node via QUIC.
2. Relay node establishes a separate connection to the destination server.
3. Traffic passes through — the relay sees encrypted blobs, not content.
4. Server sees the relay's IP. Client sees the relay's IP. Nobody sees each other.

### Relay Network Properties

- Anyone can run a relay node (separate lightweight Rust binary).
- Relay nodes can be chained (multiple hops) but typically one hop for performance.
- Server operators can run dedicated relays or use community relay nodes.
- Clients can prefer specific relays (e.g., geographically close for lower latency).

### Threat Model (Honest Assessment)

**What the relay protects against:**
- IP address exposure (doxxing)
- DDoS attacks against servers or users
- IP harvesting by malicious server operators or users
- Casual surveillance and logging

**What the relay sees:**
- Traffic patterns (when you're online, data volume)
- That client X connects to server Y (connection metadata, not content)

**What the relay does NOT protect against:**
- Nation-state level surveillance (this is a single-hop proxy, not Tor)
- Content analysis if E2EE is not used

### DDoS Protection

- Server's real IP is never exposed publicly.
- Relay nodes are the public-facing infrastructure — they can be scaled, rotated, or placed behind commercial DDoS protection.
- If a relay goes down, clients reconnect to another one. The server is untouched.

## Server Structure & Channels

### Hierarchy

```
Server
├── Category (groups channels, applies shared permission defaults)
│   ├── Text Channel
│   ├── Voice Channel
│   └── Voice + Text Channel
├── Category
│   └── ...
└── Uncategorized channels
```

### Channel Types

| Type | Description |
|---|---|
| Text | Messages, files, embeds, reactions, threads |
| Voice | Real-time voice, optional video/screenshare |
| Voice + Text | Voice channel with attached text chat |
| Announcement | Only certain roles can post, everyone reads |
| Stage | Moderated voice — speakers and listeners, hand-raising |

### Per-Channel Settings

- **Permission overrides** — who can see/read/write/speak/manage
- **Message retention policy** — auto-purge after configured duration (1 hour, 24 hours, 7 days, 30 days, 90 days, 1 year, custom, or never)
- **Slow mode** — rate limiting messages
- **NSFW flag**
- **E2EE toggle** — for private channels
- **User limit** — optional cap for voice channels

### Server-Wide Retention Default

Server operators can set a default retention policy (e.g., "all channels purge after 1 year unless overridden"). A background cleanup task purges expired messages from storage.

## Permissions & Roles

### Model

Discord-style role-based access control with server-side enforcement.

Users can have multiple roles. Permissions are additive (any role granting a permission means you have it), except explicit Deny overrides at the channel level.

### Built-in Roles (Cannot Be Deleted)

- **Owner** — Full control, cannot be restricted.
- **@everyone** — Default role every member has. Sets baseline permissions.

### Permission Resolution Order

```
Server-wide role permissions
  → Category overrides (if set)
    → Channel overrides (if set)
      = Final effective permissions
```

### Core Permissions

| Permission | Controls |
|---|---|
| View Channel | Can see the channel exists |
| Read Messages | Can read message history |
| Send Messages | Can type in the channel |
| Manage Messages | Can delete/pin others' messages |
| Connect | Can join voice channels |
| Speak | Can unmute in voice |
| Stream | Can screenshare/video |
| Manage Channel | Can edit channel settings, retention policy |
| Manage Roles | Can create/edit roles below their highest role |
| Manage Server | Server settings, categories, invites |
| Kick / Ban | Remove users (ban is by public key) |
| Admin | All permissions, can only be granted by Owner |

### Channel Override States

- **Inherit** (default) — Use whatever the role says.
- **Allow** — Explicitly grant, even if role doesn't have it.
- **Deny** — Explicitly block, even if role grants it. Deny always wins.

### Server Templates

Pre-built configurations for common use cases:

- **Gaming Community** — Admin, Moderator, Member, Guest roles. Voice lobbies, LFG, announcements.
- **Friend Group** — Flat structure, everyone can do most things.
- **Organization/Team** — Departments as categories, announcement channels, stricter defaults.
- **Public Community** — Read-heavy defaults, verified role for posting, mod tools.
- **Blank** — Start from scratch.

### Security Guarantee: Server-Side Enforcement

Every API call and WebSocket message checks permissions before responding. The server filters responses to include only what the user is authorized to see. If a user lacks View Channel permission, the server does not send the channel name, ID, or any metadata. A modded client receives exactly the same data as the official client because unauthorized data is never transmitted.

This is an architectural security principle, not a client-side UI decision. It prevents the class of attacks seen in Discord where modded clients can view hidden channel names and metadata.

## Text Chat & Messaging

### Message Features

- Rich text (markdown — bold, italic, code blocks, quotes)
- File attachments (images, videos, documents; configurable size limits per server)
- Embeds (link previews, rich media)
- Reactions (emoji)
- Replies (reply to a specific message with context)
- Threads (branch conversations off a message)
- Mentions (@user, @role, @everyone/@here with permission gating)
- Message editing and deletion (optional edit history visibility per server setting)
- Pins

### Search

- **Non-E2EE channels:** Full-text server-side search across channels the user can access.
- **E2EE channels:** Client-side search only. The client builds a local search index for decrypted history.

### Message Delivery

- Real-time via WebSocket through the relay layer.
- Offline messages queue and deliver on reconnect.
- Read receipts (optional, off by default).
- Typing indicators (optional, off by default).

### Voice Messages

- Record a voice clip in the client.
- Encoded as Opus, sent as an encrypted attachment.
- Plays inline in chat with waveform visualization.

### DMs and Group DMs

- **DMs:** Always E2EE. Work P2P through relay layer. Notification relay handles offline delivery.
- **Group DMs (up to ~10 people):** P2P mesh through relay, no server needed.
- **Beyond ~10 people:** App suggests creating a server for persistence and performance.

### Bulk Attachment Management

- **Gallery view** — Switch to a media grid in any DM or channel showing all attachments.
- **Multi-select / select all** — Download multiple attachments as a zip.
- **Filter by type** — Images, videos, audio, documents, links.
- **Filter by date range** — e.g., "all photos from March 2026."
- **Filter by user** — In groups/channels, show only one person's attachments.

### Data Deletion Rights

**Delete all messages with a specific user (DMs):**
1. User triggers "Delete all messages with [user]."
2. 72-hour grace period (cancellable).
3. Other user is notified of the pending deletion.
4. After 72 hours: all messages and attachments permanently erased from both sides.

**Delete all messages from a server ("right to be forgotten"):**
1. User triggers "Delete all my messages from [server]."
2. 72-hour grace period (cancellable).
3. Server admins notified of the pending purge.
4. After 72 hours: every message, attachment, and reaction by that user is purged from the server.

**Honest limitation:** For E2EE content, deletion removes encrypted blobs from server storage and sends a delete directive to other clients. The official client honors the directive. Modded clients could ignore it, and already-decrypted local copies cannot be force-deleted from another person's device. The server-side data is gone regardless.

## Voice & Media

### Voice Channels

- Audio streams through the relay layer to the server's SFU.
- SFU forwards audio to participants (no mixing, no transcoding).
- Opus codec, adaptive bitrate.

### Voice Features

- Per-user volume control (receiver-side)
- Server mute/deafen (moderation)
- Self mute/deafen
- Push-to-talk and voice activity detection
- Noise suppression (client-side, pre-send)
- Priority speaker (role permission — audio ducks others)
- Voice channel user limit

### Video

- Toggle camera in voice channels.
- VP9/AV1 codec.
- Simulcast — client sends multiple quality layers, SFU sends each viewer the appropriate one.
- Grid view for multiple feeds.

### Screen Sharing

- Share full screen or specific window/application.
- System audio capture included.
- Up to 1080p60 for the streamer; SFU scales for viewers.
- Multiple simultaneous screen shares per channel.
- "Watch party" mode — one share as main view, participants in sidebar.

### Stage Channels

- Speakers and listeners (moderated voice).
- Listeners can raise hand to request speaking.
- Moderators promote/demote speakers.
- Optional screen share for speakers.

### Bidirectional Quality Control

**Sender side:**
- Outgoing quality cap (720p, 1080p, 1440p, 4K).
- Upload bandwidth limit.
- Simulcast layers up to the chosen cap.

**Receiver side:**
- Per-stream quality selection (low, medium, high, auto).
- Download bandwidth limit — client distributes budget across active streams.
- Pin specific quality for specific streams.

**Auto mode (default):**
- Active speaker gets highest quality.
- Thumbnail participants get low quality.
- Adapts to bandwidth fluctuations.
- Most users never need to touch manual settings.

### Relay Routing

All voice, video, and screen share traffic routes through the relay layer. The SFU receives streams from the relay and forwards them back through the relay. Adds ~10-30ms latency, which is imperceptible for voice and acceptable for screen sharing.

Voice channels are end-to-end encrypted. Participants exchange session keys via the E2EE engine, encrypt audio/video before sending, and the SFU forwards encrypted blobs it cannot listen to. This is consistent with the platform's privacy-first philosophy — the server operator cannot eavesdrop on voice calls.

## Client Application

### Technology

- **React + TypeScript** for the UI.
- **Tauri** for desktop packaging (Windows, Mac, Linux). ~10-20MB, uses system webview.
- **Same codebase** serves the web version.
- **Mobile (future):** React Native, sharing core logic.

### Client Responsibilities

- Keypair generation, storage, and management.
- E2EE encryption/decryption (all crypto is local).
- Local search index for E2EE channels.
- Audio/video capture and processing (noise suppression, echo cancellation).
- Push-to-talk hotkey detection (background-capable).
- Notification relay connection (background process).

### UI Structure

- **Left sidebar:** Server list, DMs, group chats.
- **Second panel:** Channels/categories for the selected server.
- **Main panel:** Chat, voice, or media view.
- **Right panel:** Member list, user profiles, search results.

### Offline Capabilities

- Message drafts saved locally.
- Recently viewed channels cached for offline reading.
- Queued messages send on reconnect.
- Local search on cached/decrypted history.

### Settings

- Keybinds (push-to-talk, mute, deafen, etc.)
- Audio device selection (input, output, separate devices)
- Notification preferences (per server, per channel, per DM)
- Appearance (theme, font size, compact vs. cozy message density)
- Privacy toggles (typing indicators, read receipts, online status)
- Bandwidth controls (receiver-side quality settings)
- Accessibility (screen reader support, reduced motion, high contrast)

### Theming

- Built-in themes (dark, light, OLED).
- Custom CSS injection for power users.
- Theme sharing (export/import theme files).

## Notification Relay

A lightweight, self-hostable service for offline message delivery and push notifications.

### How It Works

1. Client registers with a notification relay on setup.
2. When a message arrives and the client is offline, the encrypted blob lands at the notification relay.
3. The relay stores the blob and fires a push notification.
4. User opens the app, pulls encrypted messages, decrypts locally.

### Platform Integration

- **Desktop:** Persistent WebSocket connection. Tauri background process shows system notifications when main window is closed.
- **Mobile (future):** Bridges to Apple APNs and Google FCM.

### Privacy

- Notification relay stores only encrypted blobs it cannot read.
- Notification metadata can be minimal (e.g., "new message" without sender or content).
- Users choose which relay to register with — a friend's, a community one, or their own.

## Server Discovery

### Default: Invite Links

- Servers are private by default.
- Join via invite link/code shared by someone with invite permissions.
- Invite links can be time-limited or usage-limited.

### Optional: Server Directory

- Self-hostable directory service.
- Server operators opt in to list their server.
- Directories are independently run — anyone can host one.
- Clients can be configured to browse multiple directories.

## Sub-Project Decomposition

This platform is too large for a single implementation cycle. The following sub-projects should be built in order, each getting its own plan and implementation cycle:

### Phase 1: Foundation (Privacy Infrastructure)
- Cryptographic identity (keypair generation, storage, signing, verification)
- Relay layer (proxy node, QUIC transport, IP masking)
- Basic client shell (Tauri + React scaffold, key management UI)
- P2P encrypted DMs through relay (the simplest end-to-end use case)
- Notification relay (offline delivery, push notifications)

### Phase 2: Servers & Text
- Server binary (single instance, SQLite storage)
- Channel and category management
- Permissions and roles (server-side enforcement)
- Text messaging (real-time via WebSocket, history, search)
- Server templates
- Invite links

### Phase 3: Rich Messaging
- File attachments and media embeds
- Threads, replies, reactions, pins
- Bulk attachment management (gallery view, multi-download)
- Data deletion rights (72h grace period purge)
- Message retention policies (auto-purge)
- Voice messages

### Phase 4: Voice & Media
- SFU integration (Opus voice, WebRTC)
- Voice channels with relay routing
- Video (simulcast, VP9/AV1)
- Screen sharing (window capture, system audio)
- Bidirectional quality control
- Stage channels

### Phase 5: Polish & Ecosystem
- Server directory (self-hostable)
- Theming system (custom CSS, theme sharing)
- Group DMs (P2P mesh)
- Mobile client (React Native)
- Accessibility audit
- Performance optimization

## Resolved Decisions

1. **Project name** — Farder. Binary, protocol, and product.
2. **Bot/integration API** — Yes. Webhooks, bot accounts with keypairs, and mIRC-style plugin system. All three supported.
3. **Federation between servers** — No. Servers are independent. No shared channels across servers.
4. **Moderation tooling** — In scope. Spam detection, automated moderation rules, audit logs.
5. **Licensing** — AGPLv3 (open source, copyleft). Business model: hosted relay network, managed hosting ("Farder Cloud"), premium features, enterprise dual-licensing.
6. **E2EE for voice** — Yes. Voice channels are end-to-end encrypted, consistent with the platform's privacy-first philosophy. The SFU forwards encrypted audio blobs it cannot listen to.
7. **Relay node bootstrap** — Hybrid approach. Farder ships with default relay nodes. Invite links can specify a preferred relay. Server operators can recommend specific relays. Users can manually configure relays.

8. **Protocol specification** — Self-documenting approach. The wire protocol is documented as each component is built. By end of Phase 1, the protocol spec exists as a natural byproduct of development.
9. **Key revocation** — PIN-based revocation. At keypair creation, user sets a 4-digit recovery PIN. To revoke a compromised key: present the old public key + PIN from a new device with a new keypair. Server verifies PIN, marks old key as revoked, propagates a signed revocation notice to all servers that have seen the old key. Simple, no compromised private key needed.

## Open Questions

None. All design decisions resolved.
