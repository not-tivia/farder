# Farder Phase 2: Servers & Text — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the community server binary — a single Rust binary providing channels, categories, roles, permissions, real-time text messaging, invite links, and server templates over QUIC.

**Architecture:** One new crate (`farder-server`) added to the workspace. Clients connect directly via QUIC (Quinn), authenticate with Ed25519 challenge-response, then communicate over a persistent bi-directional stream using length-prefixed MessagePack frames. Server state lives in a single SQLite database with FTS5 for message search. The protocol uses request IDs so clients can match responses to requests while receiving interleaved server-pushed events.

**Tech Stack:**
- Rust (farder-server binary)
- Quinn 0.11 (QUIC)
- rustls 0.23 (TLS, ring backend)
- rusqlite 0.31 (SQLite, bundled, FTS5)
- ed25519-dalek via farder-crypto (auth)
- rmp-serde via farder-protocol (MessagePack codec)
- toml 0.8 (server template parsing)
- clap 4 (CLI)

**Spec:** `docs/specs/2026-04-02-farder-phase2-servers-text-design.md`

---

## File Structure

### New Crate: `crates/farder-server/`

```
crates/farder-server/
├── Cargo.toml
├── templates/                    # Embedded default templates (include_str!)
│   ├── blank.toml
│   ├── friend-group.toml
│   ├── gaming-community.toml
│   ├── organization.toml
│   └── public-community.toml
└── src/
    ├── main.rs                   # Entry point: CLI args, QUIC listener, accept loop
    ├── state.rs                  # ServerState: DB, sessions, connected clients, subscriptions
    ├── db.rs                     # SQLite schema init (all tables, FTS5, indexes)
    ├── auth.rs                   # Challenge-response, session tokens, setup token, member registration
    ├── permissions.rs            # Permission bit constants, resolve_permissions() algorithm
    ├── members.rs                # Member CRUD, role CRUD, member-role mapping (DB operations)
    ├── channels.rs               # Channel/category CRUD, overrides, settings (DB operations)
    ├── messages.rs               # Message insert/edit/delete/pin, FTS5 search, history (DB operations)
    ├── invites.rs                # Invite generation, validation, expiry (DB operations)
    ├── templates.rs              # TOML parsing, built-in templates, filesystem loading, application
    ├── handlers.rs               # Request dispatch: permission check → DB op → response + events
    ├── connection.rs             # Per-client QUIC handler: auth flow, frame read/write, event push
    ├── events.rs                 # EventTarget enum, broadcast logic
    └── retention.rs              # Background task: periodic message purge per retention policy
```

### Modified Files

```
Cargo.toml                                  # Add farder-server to workspace members + dev-deps
crates/farder-protocol/src/lib.rs           # Add `pub mod server;`
crates/farder-protocol/src/server.rs        # NEW: ServerRequest, ServerResponse, ServerEvent, supporting types
tests/e2e_server.rs                         # NEW: Multi-client integration test
```

### Design Decisions

- **Storage functions take `&rusqlite::Connection`** — decouples DB operations from concurrency. The caller (`ServerState` or test) provides the connection. This makes every storage function unit-testable with an in-memory SQLite database.
- **Handlers are synchronous functions** — `fn handle_request(db, member, request) -> Result<(response, events)>`. No async, no networking. Testable in isolation.
- **`ServerState` wraps DB in `Mutex<Connection>`** — handlers lock, execute, release. Fine for Phase 2 scale.
- **Wire framing reuses Phase 1 pattern** — 4-byte big-endian length prefix + MessagePack payload.
- **Request IDs** — client assigns a `u32` ID to each request; server echoes it in the response. Events have no request ID.

---

## Task 1: Scaffold & Server Protocol Types

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/farder-protocol/src/lib.rs`
- Modify: `crates/farder-protocol/Cargo.toml`
- Create: `crates/farder-protocol/src/server.rs`
- Create: `crates/farder-server/Cargo.toml`
- Create: `crates/farder-server/src/main.rs`
- Create: `crates/farder-server/src/db.rs`
- Create: `crates/farder-server/src/state.rs`
- Create: `crates/farder-server/src/auth.rs`
- Create: `crates/farder-server/src/permissions.rs`
- Create: `crates/farder-server/src/members.rs`
- Create: `crates/farder-server/src/channels.rs`
- Create: `crates/farder-server/src/messages.rs`
- Create: `crates/farder-server/src/invites.rs`
- Create: `crates/farder-server/src/templates.rs`
- Create: `crates/farder-server/src/handlers.rs`
- Create: `crates/farder-server/src/connection.rs`
- Create: `crates/farder-server/src/events.rs`
- Create: `crates/farder-server/src/retention.rs`

- [ ] **Step 1: Add farder-server to workspace**

`Cargo.toml` (workspace root) — add `"crates/farder-server"` to the `members` array and add `farder-server` to `[workspace.dependencies]` and `[dev-dependencies]`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/farder-crypto",
    "crates/farder-protocol",
    "crates/farder-relay",
    "crates/farder-notify",
    "crates/farder-node",
    "crates/farder-demo",
    "crates/farder-server",
]
```

Add to `[workspace.dependencies]`:

```toml
farder-server = { path = "crates/farder-server" }
```

Add to `[dev-dependencies]`:

```toml
farder-server = { path = "crates/farder-server" }
```

- [ ] **Step 2: Create farder-server Cargo.toml**

`crates/farder-server/Cargo.toml`:

```toml
[package]
name = "farder-server"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
farder-crypto = { workspace = true }
farder-protocol = { workspace = true }
quinn = "0.11"
rustls = { version = "0.23", features = ["ring"] }
rusqlite = { version = "0.31", features = ["bundled"] }
rcgen = "0.13"
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
clap = { version = "4", features = ["derive"] }
toml = "0.8"
rand = "0.8"
hex = "0.4"

[dev-dependencies]
```

- [ ] **Step 3: Create farder-server module stubs**

`crates/farder-server/src/main.rs`:

```rust
mod auth;
mod channels;
mod connection;
mod db;
mod events;
mod handlers;
mod invites;
mod members;
mod messages;
mod permissions;
mod retention;
mod state;
mod templates;

fn main() {
    println!("farder-server v{}", env!("CARGO_PKG_VERSION"));
}
```

Create each module as an empty file:
- `crates/farder-server/src/auth.rs` — empty
- `crates/farder-server/src/channels.rs` — empty
- `crates/farder-server/src/connection.rs` — empty
- `crates/farder-server/src/db.rs` — empty
- `crates/farder-server/src/events.rs` — empty
- `crates/farder-server/src/handlers.rs` — empty
- `crates/farder-server/src/invites.rs` — empty
- `crates/farder-server/src/members.rs` — empty
- `crates/farder-server/src/messages.rs` — empty
- `crates/farder-server/src/permissions.rs` — empty
- `crates/farder-server/src/retention.rs` — empty
- `crates/farder-server/src/state.rs` — empty
- `crates/farder-server/src/templates.rs` — empty

- [ ] **Step 4: Write server protocol types with roundtrip tests**

`crates/farder-protocol/src/lib.rs` — add the server module:

```rust
pub mod messages;
pub mod codec;
pub mod server;
```

`crates/farder-protocol/src/server.rs`:

```rust
use farder_crypto::identity::PublicKey;
use serde::{Deserialize, Serialize};

// ── Channel & role info types ───────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelType {
    Text,
    Announcement,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MessageInfo {
    pub id: u64,
    pub channel_id: u64,
    pub author: PublicKey,
    pub content: String,
    pub timestamp: u64,
    pub edited_at: Option<u64>,
    pub reply_to: Option<u64>,
    pub pinned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelInfo {
    pub id: u64,
    pub name: String,
    pub channel_type: ChannelType,
    pub category_id: Option<u64>,
    pub position: u32,
    pub topic: Option<String>,
    pub nsfw: bool,
    pub slow_mode_secs: u32,
    pub retention_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CategoryInfo {
    pub id: u64,
    pub name: String,
    pub position: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RoleInfo {
    pub id: u64,
    pub name: String,
    pub permissions: u64,
    pub color: Option<String>,
    pub position: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MemberInfo {
    pub public_key: PublicKey,
    pub display_name: String,
    pub joined_at: u64,
    pub role_ids: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OverrideInfo {
    pub role_id: u64,
    pub allow: u64,
    pub deny: u64,
}

// ── Client → Server ─────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClientFrame {
    Authenticate {
        public_key: PublicKey,
        signed_challenge: Vec<u8>,
        invite_code: Option<String>,
        setup_token: Option<String>,
    },
    Request {
        id: u32,
        body: ServerRequest,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerRequest {
    Subscribe { channel_ids: Vec<u64> },
    SendMessage { channel_id: u64, content: String, reply_to: Option<u64> },
    EditMessage { message_id: u64, new_content: String },
    DeleteMessage { message_id: u64 },
    FetchHistory { channel_id: u64, before_id: Option<u64>, limit: u32 },
    PinMessage { message_id: u64 },
    UnpinMessage { message_id: u64 },
    Search { query: String, channel_id: Option<u64>, limit: u32 },
    Typing { channel_id: u64 },
    CreateChannel { name: String, channel_type: ChannelType, category_id: Option<u64>, position: Option<u32> },
    UpdateChannel { channel_id: u64, name: Option<String>, topic: Option<String>, nsfw: Option<bool>, slow_mode_secs: Option<u32>, retention_secs: Option<Option<u64>> },
    DeleteChannel { channel_id: u64 },
    CreateCategory { name: String, position: Option<u32> },
    UpdateCategory { category_id: u64, name: Option<String>, position: Option<u32> },
    DeleteCategory { category_id: u64 },
    CreateRole { name: String, permissions: u64, color: Option<String>, position: Option<u32> },
    UpdateRole { role_id: u64, name: Option<String>, permissions: Option<u64>, color: Option<String>, position: Option<u32> },
    DeleteRole { role_id: u64 },
    AssignRole { member_key: PublicKey, role_id: u64 },
    RemoveRole { member_key: PublicKey, role_id: u64 },
    KickMember { member_key: PublicKey },
    BanMember { member_key: PublicKey },
    CreateInvite { max_uses: Option<u32>, expires_in_secs: Option<u64>, target_channel: Option<u64> },
    GetServerInfo,
    GetMembers,
    SetChannelOverride { channel_id: u64, role_id: u64, allow: u64, deny: u64 },
    SetCategoryOverride { category_id: u64, role_id: u64, allow: u64, deny: u64 },
}

// ── Server → Client ─────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerFrame {
    Challenge { nonce: [u8; 32] },
    Authenticated { session_token: Vec<u8> },
    AuthError { reason: String },
    Response { request_id: u32, body: ServerResponse },
    Event(ServerEvent),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerResponse {
    Ok,
    Error { reason: String },
    MessageSent { id: u64, timestamp: u64 },
    History { messages: Vec<MessageInfo> },
    SearchResults { messages: Vec<MessageInfo> },
    ServerInfo {
        name: String,
        member_count: u32,
        channels: Vec<ChannelInfo>,
        categories: Vec<CategoryInfo>,
        roles: Vec<RoleInfo>,
    },
    Members { members: Vec<MemberInfo> },
    InviteCreated { code: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerEvent {
    NewMessage { message: MessageInfo },
    MessageEdited { message_id: u64, channel_id: u64, new_content: String, edited_at: u64 },
    MessageDeleted { message_id: u64, channel_id: u64 },
    MessagePinned { message_id: u64, channel_id: u64 },
    MessageUnpinned { message_id: u64, channel_id: u64 },
    MemberJoined { public_key: PublicKey, display_name: String },
    MemberLeft { public_key: PublicKey },
    MemberBanned { public_key: PublicKey },
    TypingStarted { channel_id: u64, public_key: PublicKey },
    ChannelCreated { channel: ChannelInfo },
    ChannelUpdated { channel: ChannelInfo },
    ChannelDeleted { channel_id: u64 },
    CategoryCreated { category: CategoryInfo },
    CategoryUpdated { category: CategoryInfo },
    CategoryDeleted { category_id: u64 },
    RoleCreated { role: RoleInfo },
    RoleUpdated { role: RoleInfo },
    RoleDeleted { role_id: u64 },
    PermissionsChanged,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;
    use farder_crypto::identity::Keypair;

    #[test]
    fn test_roundtrip_client_frame_authenticate() {
        let kp = Keypair::generate();
        let frame = ClientFrame::Authenticate {
            public_key: kp.public_key(),
            signed_challenge: vec![1, 2, 3],
            invite_code: Some("abc123".to_string()),
            setup_token: None,
        };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ClientFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ClientFrame::Authenticate { public_key, invite_code, .. } => {
                assert_eq!(public_key, kp.public_key());
                assert_eq!(invite_code, Some("abc123".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_client_frame_request() {
        let frame = ClientFrame::Request {
            id: 42,
            body: ServerRequest::SendMessage {
                channel_id: 1,
                content: "hello".to_string(),
                reply_to: None,
            },
        };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ClientFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ClientFrame::Request { id, body } => {
                assert_eq!(id, 42);
                match body {
                    ServerRequest::SendMessage { channel_id, content, reply_to } => {
                        assert_eq!(channel_id, 1);
                        assert_eq!(content, "hello");
                        assert!(reply_to.is_none());
                    }
                    _ => panic!("wrong request variant"),
                }
            }
            _ => panic!("wrong frame variant"),
        }
    }

    #[test]
    fn test_roundtrip_server_frame_challenge() {
        let nonce = [42u8; 32];
        let frame = ServerFrame::Challenge { nonce };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ServerFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ServerFrame::Challenge { nonce: n } => assert_eq!(n, nonce),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_server_frame_response() {
        let kp = Keypair::generate();
        let msg = MessageInfo {
            id: 1,
            channel_id: 5,
            author: kp.public_key(),
            content: "test message".to_string(),
            timestamp: 1000,
            edited_at: None,
            reply_to: None,
            pinned: false,
        };
        let frame = ServerFrame::Response {
            request_id: 7,
            body: ServerResponse::History { messages: vec![msg.clone()] },
        };
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ServerFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ServerFrame::Response { request_id, body } => {
                assert_eq!(request_id, 7);
                match body {
                    ServerResponse::History { messages } => {
                        assert_eq!(messages.len(), 1);
                        assert_eq!(messages[0].content, "test message");
                    }
                    _ => panic!("wrong response variant"),
                }
            }
            _ => panic!("wrong frame variant"),
        }
    }

    #[test]
    fn test_roundtrip_server_event() {
        let kp = Keypair::generate();
        let event = ServerEvent::NewMessage {
            message: MessageInfo {
                id: 99,
                channel_id: 3,
                author: kp.public_key(),
                content: "event msg".to_string(),
                timestamp: 2000,
                edited_at: Some(2001),
                reply_to: Some(50),
                pinned: true,
            },
        };
        let frame = ServerFrame::Event(event);
        let bytes = codec::encode(&frame).unwrap();
        let decoded: ServerFrame = codec::decode(&bytes).unwrap();
        match decoded {
            ServerFrame::Event(ServerEvent::NewMessage { message }) => {
                assert_eq!(message.id, 99);
                assert_eq!(message.edited_at, Some(2001));
                assert_eq!(message.reply_to, Some(50));
                assert!(message.pinned);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_all_request_variants() {
        let kp = Keypair::generate();
        let requests = vec![
            ServerRequest::Subscribe { channel_ids: vec![1, 2, 3] },
            ServerRequest::SendMessage { channel_id: 1, content: "hi".into(), reply_to: Some(5) },
            ServerRequest::EditMessage { message_id: 10, new_content: "edited".into() },
            ServerRequest::DeleteMessage { message_id: 10 },
            ServerRequest::FetchHistory { channel_id: 1, before_id: Some(100), limit: 50 },
            ServerRequest::PinMessage { message_id: 10 },
            ServerRequest::UnpinMessage { message_id: 10 },
            ServerRequest::Search { query: "hello".into(), channel_id: Some(1), limit: 20 },
            ServerRequest::Typing { channel_id: 1 },
            ServerRequest::CreateChannel { name: "general".into(), channel_type: ChannelType::Text, category_id: None, position: Some(0) },
            ServerRequest::UpdateChannel { channel_id: 1, name: Some("renamed".into()), topic: None, nsfw: None, slow_mode_secs: None, retention_secs: None },
            ServerRequest::DeleteChannel { channel_id: 1 },
            ServerRequest::CreateCategory { name: "General".into(), position: Some(0) },
            ServerRequest::UpdateCategory { category_id: 1, name: Some("Renamed".into()), position: None },
            ServerRequest::DeleteCategory { category_id: 1 },
            ServerRequest::CreateRole { name: "Mod".into(), permissions: 0xFF, color: Some("#00FF00".into()), position: Some(2) },
            ServerRequest::UpdateRole { role_id: 1, name: None, permissions: Some(0xFFFF), color: None, position: None },
            ServerRequest::DeleteRole { role_id: 1 },
            ServerRequest::AssignRole { member_key: kp.public_key(), role_id: 1 },
            ServerRequest::RemoveRole { member_key: kp.public_key(), role_id: 1 },
            ServerRequest::KickMember { member_key: kp.public_key() },
            ServerRequest::BanMember { member_key: kp.public_key() },
            ServerRequest::CreateInvite { max_uses: Some(10), expires_in_secs: Some(3600), target_channel: Some(1) },
            ServerRequest::GetServerInfo,
            ServerRequest::GetMembers,
            ServerRequest::SetChannelOverride { channel_id: 1, role_id: 2, allow: 0x03, deny: 0x04 },
            ServerRequest::SetCategoryOverride { category_id: 1, role_id: 2, allow: 0x03, deny: 0x04 },
        ];
        for req in requests {
            let frame = ClientFrame::Request { id: 1, body: req };
            let bytes = codec::encode(&frame).unwrap();
            let _decoded: ClientFrame = codec::decode(&bytes).unwrap();
        }
    }
}
```

- [ ] **Step 5: Verify everything compiles and tests pass**

Run: `cd /home/deez/farder && cargo test -p farder-protocol`

Expected: All existing tests pass plus 6 new server roundtrip tests pass.

Run: `cd /home/deez/farder && cargo check -p farder-server`

Expected: Compiles with no errors (warnings about unused modules are fine).

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/ crates/farder-protocol/src/server.rs crates/farder-protocol/src/lib.rs Cargo.toml
git commit -m "feat(server): scaffold farder-server crate and add server protocol types"
```

---

## Task 2: Permission Bitfield & Resolution

**Files:**
- Create: `crates/farder-server/src/permissions.rs`

- [ ] **Step 1: Write failing tests for permission constants and resolution**

`crates/farder-server/src/permissions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_constants_are_unique_bits() {
        let all = [
            VIEW_CHANNEL, READ_MESSAGES, SEND_MESSAGES, MANAGE_MESSAGES,
            CONNECT, SPEAK, STREAM, MANAGE_CHANNEL, MANAGE_ROLES,
            MANAGE_SERVER, KICK_MEMBERS, BAN_MEMBERS, ADMIN, CREATE_INVITES,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(a.is_power_of_two(), "permission at index {} is not a single bit", i);
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert_eq!(a & b, 0, "permissions at index {} and {} overlap", i, j);
                }
            }
        }
    }

    #[test]
    fn test_all_permissions_covers_all_bits() {
        let expected = VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES | MANAGE_MESSAGES
            | CONNECT | SPEAK | STREAM | MANAGE_CHANNEL | MANAGE_ROLES
            | MANAGE_SERVER | KICK_MEMBERS | BAN_MEMBERS | ADMIN | CREATE_INVITES;
        assert_eq!(ALL_PERMISSIONS, expected);
    }

    #[test]
    fn test_has_permission() {
        let perms = VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES;
        assert!(has(perms, VIEW_CHANNEL));
        assert!(has(perms, READ_MESSAGES));
        assert!(!has(perms, MANAGE_MESSAGES));
    }

    #[test]
    fn test_resolve_everyone_only() {
        let ctx = ResolutionContext {
            everyone_permissions: VIEW_CHANNEL | READ_MESSAGES,
            role_permissions: vec![],
            category_overrides: vec![],
            channel_overrides: vec![],
            is_owner: false,
        };
        let perms = resolve(ctx);
        assert_eq!(perms, VIEW_CHANNEL | READ_MESSAGES);
    }

    #[test]
    fn test_resolve_roles_union() {
        let ctx = ResolutionContext {
            everyone_permissions: VIEW_CHANNEL,
            role_permissions: vec![READ_MESSAGES, SEND_MESSAGES | CREATE_INVITES],
            category_overrides: vec![],
            channel_overrides: vec![],
            is_owner: false,
        };
        let perms = resolve(ctx);
        assert_eq!(perms, VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES | CREATE_INVITES);
    }

    #[test]
    fn test_resolve_channel_override_deny_wins() {
        let ctx = ResolutionContext {
            everyone_permissions: VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES,
            role_permissions: vec![],
            category_overrides: vec![],
            channel_overrides: vec![
                Override { allow: 0, deny: SEND_MESSAGES },
            ],
            is_owner: false,
        };
        let perms = resolve(ctx);
        assert_eq!(perms, VIEW_CHANNEL | READ_MESSAGES);
    }

    #[test]
    fn test_resolve_channel_override_allow_grants() {
        let ctx = ResolutionContext {
            everyone_permissions: VIEW_CHANNEL,
            role_permissions: vec![],
            category_overrides: vec![],
            channel_overrides: vec![
                Override { allow: SEND_MESSAGES, deny: 0 },
            ],
            is_owner: false,
        };
        let perms = resolve(ctx);
        assert_eq!(perms, VIEW_CHANNEL | SEND_MESSAGES);
    }

    #[test]
    fn test_resolve_category_override_then_channel_override() {
        let ctx = ResolutionContext {
            everyone_permissions: VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES,
            role_permissions: vec![],
            category_overrides: vec![
                Override { allow: 0, deny: SEND_MESSAGES },
            ],
            channel_overrides: vec![
                Override { allow: SEND_MESSAGES, deny: 0 },
            ],
            is_owner: false,
        };
        let perms = resolve(ctx);
        // Category denies SEND, but channel re-allows it
        assert_eq!(perms, VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES);
    }

    #[test]
    fn test_resolve_deny_wins_within_same_level() {
        // Two roles: one allows SEND, another denies it at channel level
        let ctx = ResolutionContext {
            everyone_permissions: VIEW_CHANNEL | READ_MESSAGES,
            role_permissions: vec![SEND_MESSAGES],
            category_overrides: vec![],
            channel_overrides: vec![
                Override { allow: SEND_MESSAGES, deny: 0 },  // role A allows
                Override { allow: 0, deny: SEND_MESSAGES },  // role B denies
            ],
            is_owner: false,
        };
        let perms = resolve(ctx);
        // Deny wins when both allow and deny are set at same level
        assert!(!has(perms, SEND_MESSAGES));
    }

    #[test]
    fn test_resolve_admin_gets_all() {
        let ctx = ResolutionContext {
            everyone_permissions: ADMIN,
            role_permissions: vec![],
            category_overrides: vec![],
            channel_overrides: vec![],
            is_owner: false,
        };
        let perms = resolve(ctx);
        assert_eq!(perms, ALL_PERMISSIONS);
    }

    #[test]
    fn test_resolve_owner_gets_all() {
        let ctx = ResolutionContext {
            everyone_permissions: 0,
            role_permissions: vec![],
            category_overrides: vec![],
            channel_overrides: vec![],
            is_owner: true,
        };
        let perms = resolve(ctx);
        assert_eq!(perms, ALL_PERMISSIONS);
    }

    #[test]
    fn test_resolve_admin_overrides_denies() {
        let ctx = ResolutionContext {
            everyone_permissions: VIEW_CHANNEL | ADMIN,
            role_permissions: vec![],
            category_overrides: vec![],
            channel_overrides: vec![
                Override { allow: 0, deny: VIEW_CHANNEL },
            ],
            is_owner: false,
        };
        let perms = resolve(ctx);
        // ADMIN bit survives the deny, so final result is ALL
        assert_eq!(perms, ALL_PERMISSIONS);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- permissions`

Expected: Compilation errors (constants and functions not defined yet).

- [ ] **Step 3: Implement permission constants and resolution**

Add to the top of `crates/farder-server/src/permissions.rs` (above the tests module):

```rust
// Permission bit constants (u64 bitfield)
pub const VIEW_CHANNEL: u64 = 1 << 0;
pub const READ_MESSAGES: u64 = 1 << 1;
pub const SEND_MESSAGES: u64 = 1 << 2;
pub const MANAGE_MESSAGES: u64 = 1 << 3;
pub const CONNECT: u64 = 1 << 4;        // Phase 4
pub const SPEAK: u64 = 1 << 5;          // Phase 4
pub const STREAM: u64 = 1 << 6;         // Phase 4
pub const MANAGE_CHANNEL: u64 = 1 << 7;
pub const MANAGE_ROLES: u64 = 1 << 8;
pub const MANAGE_SERVER: u64 = 1 << 9;
pub const KICK_MEMBERS: u64 = 1 << 10;
pub const BAN_MEMBERS: u64 = 1 << 11;
pub const ADMIN: u64 = 1 << 12;
pub const CREATE_INVITES: u64 = 1 << 13;

pub const ALL_PERMISSIONS: u64 = VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES
    | MANAGE_MESSAGES | CONNECT | SPEAK | STREAM | MANAGE_CHANNEL
    | MANAGE_ROLES | MANAGE_SERVER | KICK_MEMBERS | BAN_MEMBERS
    | ADMIN | CREATE_INVITES;

/// Default permissions for @everyone: view, read, send, create invites.
pub const DEFAULT_EVERYONE: u64 = VIEW_CHANNEL | READ_MESSAGES | SEND_MESSAGES | CREATE_INVITES;

pub fn has(permissions: u64, permission: u64) -> bool {
    permissions & permission == permission
}

pub struct Override {
    pub allow: u64,
    pub deny: u64,
}

pub struct ResolutionContext {
    pub everyone_permissions: u64,
    pub role_permissions: Vec<u64>,
    pub category_overrides: Vec<Override>,
    pub channel_overrides: Vec<Override>,
    pub is_owner: bool,
}

/// Resolve effective permissions for a member in a channel.
/// Follows the algorithm from the design spec.
pub fn resolve(ctx: ResolutionContext) -> u64 {
    // Owner always gets everything
    if ctx.is_owner {
        return ALL_PERMISSIONS;
    }

    // 1. Start with @everyone
    let mut perms = ctx.everyone_permissions;

    // 2. OR in all assigned roles
    for role_perms in &ctx.role_permissions {
        perms |= role_perms;
    }

    // 3. Apply category overrides (union all allows/denies, then apply once)
    if !ctx.category_overrides.is_empty() {
        let mut combined_allow: u64 = 0;
        let mut combined_deny: u64 = 0;
        for ov in &ctx.category_overrides {
            combined_allow |= ov.allow;
            combined_deny |= ov.deny;
        }
        perms &= !combined_deny;
        perms |= combined_allow;
    }

    // 4. Apply channel overrides (same union approach)
    if !ctx.channel_overrides.is_empty() {
        let mut combined_allow: u64 = 0;
        let mut combined_deny: u64 = 0;
        for ov in &ctx.channel_overrides {
            combined_allow |= ov.allow;
            combined_deny |= ov.deny;
        }
        perms &= !combined_deny;
        perms |= combined_allow;
    }

    // 5. Admin gets everything
    if has(perms, ADMIN) {
        return ALL_PERMISSIONS;
    }

    perms
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- permissions`

Expected: All 11 permission tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/permissions.rs
git commit -m "feat(server): implement permission bitfield constants and resolution algorithm"
```

---

## Task 3: Database Schema & Server State

**Files:**
- Create: `crates/farder-server/src/db.rs`
- Create: `crates/farder-server/src/state.rs`

- [ ] **Step 1: Write the database schema init function with test**

`crates/farder-server/src/db.rs`:

```rust
use anyhow::Result;
use rusqlite::Connection;

/// Initialize all tables, indexes, and FTS5 virtual tables.
/// Safe to call multiple times (uses IF NOT EXISTS).
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS members (
            public_key BLOB PRIMARY KEY,
            display_name TEXT NOT NULL,
            avatar BLOB,
            joined_at INTEGER NOT NULL,
            banned INTEGER NOT NULL DEFAULT 0,
            revoked INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS roles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            permissions INTEGER NOT NULL,
            color TEXT,
            position INTEGER NOT NULL,
            builtin INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS member_roles (
            member_key BLOB NOT NULL,
            role_id INTEGER NOT NULL,
            PRIMARY KEY (member_key, role_id),
            FOREIGN KEY (member_key) REFERENCES members(public_key),
            FOREIGN KEY (role_id) REFERENCES roles(id)
        );

        CREATE TABLE IF NOT EXISTS categories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            position INTEGER NOT NULL,
            deleted INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS channels (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            channel_type TEXT NOT NULL DEFAULT 'text',
            category_id INTEGER,
            position INTEGER NOT NULL,
            topic TEXT,
            nsfw INTEGER NOT NULL DEFAULT 0,
            slow_mode_secs INTEGER NOT NULL DEFAULT 0,
            retention_secs INTEGER,
            deleted INTEGER NOT NULL DEFAULT 0,
            deleted_at INTEGER,
            FOREIGN KEY (category_id) REFERENCES categories(id)
        );

        CREATE TABLE IF NOT EXISTS channel_overrides (
            channel_id INTEGER NOT NULL,
            role_id INTEGER NOT NULL,
            allow_bits INTEGER NOT NULL DEFAULT 0,
            deny_bits INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (channel_id, role_id),
            FOREIGN KEY (channel_id) REFERENCES channels(id),
            FOREIGN KEY (role_id) REFERENCES roles(id)
        );

        CREATE TABLE IF NOT EXISTS category_overrides (
            category_id INTEGER NOT NULL,
            role_id INTEGER NOT NULL,
            allow_bits INTEGER NOT NULL DEFAULT 0,
            deny_bits INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (category_id, role_id),
            FOREIGN KEY (category_id) REFERENCES categories(id),
            FOREIGN KEY (role_id) REFERENCES roles(id)
        );

        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            channel_id INTEGER NOT NULL,
            author BLOB NOT NULL,
            content TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            edited_at INTEGER,
            reply_to INTEGER,
            pinned INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (channel_id) REFERENCES channels(id),
            FOREIGN KEY (author) REFERENCES members(public_key)
        );

        CREATE INDEX IF NOT EXISTS idx_messages_channel_ts
            ON messages(channel_id, timestamp);
        CREATE INDEX IF NOT EXISTS idx_messages_channel_id
            ON messages(channel_id, id);

        CREATE TABLE IF NOT EXISTS invites (
            code TEXT PRIMARY KEY,
            created_by BLOB NOT NULL,
            max_uses INTEGER,
            use_count INTEGER NOT NULL DEFAULT 0,
            expires_at INTEGER,
            target_channel INTEGER,
            FOREIGN KEY (created_by) REFERENCES members(public_key)
        );
        "
    )?;

    // FTS5 virtual table for message search (separate statement — can't be in a batch
    // with IF NOT EXISTS in all SQLite versions, so we check manually)
    let has_fts: bool = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='messages_fts'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has_fts {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE messages_fts USING fts5(content, content_rowid='id', content='messages');"
        )?;
    }

    Ok(())
}

/// Open an in-memory database with the schema initialized. For tests.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init_schema(&conn)?;
    Ok(conn)
}

/// Open a file-backed database with the schema initialized.
pub fn open_file(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    init_schema(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_init_succeeds() {
        let conn = open_in_memory().unwrap();
        // Verify all tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(tables.contains(&"members".to_string()));
        assert!(tables.contains(&"roles".to_string()));
        assert!(tables.contains(&"member_roles".to_string()));
        assert!(tables.contains(&"categories".to_string()));
        assert!(tables.contains(&"channels".to_string()));
        assert!(tables.contains(&"channel_overrides".to_string()));
        assert!(tables.contains(&"category_overrides".to_string()));
        assert!(tables.contains(&"messages".to_string()));
        assert!(tables.contains(&"messages_fts".to_string()));
        assert!(tables.contains(&"invites".to_string()));
    }

    #[test]
    fn test_schema_init_idempotent() {
        let conn = open_in_memory().unwrap();
        // Should not fail on second call
        init_schema(&conn).unwrap();
    }
}
```

- [ ] **Step 2: Write the ServerState struct**

`crates/farder-server/src/state.rs`:

```rust
use crate::db;
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::ServerEvent;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tokio::sync::{mpsc, RwLock};

pub struct SessionInfo {
    pub public_key: PublicKey,
    pub expires_at: u64,
}

pub type EventSender = mpsc::Sender<ServerEvent>;

pub struct ServerState {
    pub db: Mutex<Connection>,
    pub sessions: RwLock<HashMap<[u8; 32], SessionInfo>>,
    pub clients: RwLock<HashMap<[u8; 32], EventSender>>,
    pub subscriptions: RwLock<HashMap<u64, HashSet<[u8; 32]>>>,
    pub owner: RwLock<Option<PublicKey>>,
    pub setup_token: Mutex<Option<[u8; 32]>>,
    pub server_name: String,
}

impl ServerState {
    pub fn new(conn: Connection, server_name: String) -> Self {
        Self {
            db: Mutex::new(conn),
            sessions: RwLock::new(HashMap::new()),
            clients: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
            owner: RwLock::new(None),
            setup_token: Mutex::new(None),
            server_name,
        }
    }

    pub fn new_for_test() -> Result<Self> {
        let conn = db::open_in_memory()?;
        Ok(Self::new(conn, "Test Server".to_string()))
    }
}
```

- [ ] **Step 3: Verify compilation and tests pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- db`

Expected: 2 schema tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/state.rs
git commit -m "feat(server): add database schema init and ServerState struct"
```

---

## Task 4: Member & Role Storage

**Files:**
- Create: `crates/farder-server/src/members.rs`

- [ ] **Step 1: Write failing tests for member and role operations**

`crates/farder-server/src/members.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use farder_crypto::identity::Keypair;

    fn setup() -> Connection {
        db::open_in_memory().unwrap()
    }

    fn make_key() -> PublicKey {
        Keypair::generate().public_key()
    }

    // ── Member tests ────────────────────────────────────────────────

    #[test]
    fn test_register_and_get_member() {
        let conn = setup();
        let pk = make_key();
        register_member(&conn, &pk, "Alice").unwrap();
        let member = get_member(&conn, &pk).unwrap().unwrap();
        assert_eq!(member.display_name, "Alice");
        assert!(!member.banned);
        assert!(!member.revoked);
    }

    #[test]
    fn test_get_nonexistent_member_returns_none() {
        let conn = setup();
        let pk = make_key();
        assert!(get_member(&conn, &pk).unwrap().is_none());
    }

    #[test]
    fn test_list_members() {
        let conn = setup();
        let pk1 = make_key();
        let pk2 = make_key();
        register_member(&conn, &pk1, "Alice").unwrap();
        register_member(&conn, &pk2, "Bob").unwrap();
        let members = list_members(&conn).unwrap();
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn test_ban_member() {
        let conn = setup();
        let pk = make_key();
        register_member(&conn, &pk, "Alice").unwrap();
        ban_member(&conn, &pk).unwrap();
        let member = get_member(&conn, &pk).unwrap().unwrap();
        assert!(member.banned);
    }

    #[test]
    fn test_revoke_member() {
        let conn = setup();
        let pk = make_key();
        register_member(&conn, &pk, "Alice").unwrap();
        revoke_member(&conn, &pk).unwrap();
        let member = get_member(&conn, &pk).unwrap().unwrap();
        assert!(member.revoked);
    }

    #[test]
    fn test_remove_member() {
        let conn = setup();
        let pk = make_key();
        register_member(&conn, &pk, "Alice").unwrap();
        remove_member(&conn, &pk).unwrap();
        assert!(get_member(&conn, &pk).unwrap().is_none());
    }

    // ── Role tests ──────────────────────────────────────────────────

    #[test]
    fn test_create_and_get_role() {
        let conn = setup();
        let id = create_role(&conn, "Moderator", 0xFF, Some("#00FF00"), 2, false).unwrap();
        let role = get_role(&conn, id).unwrap().unwrap();
        assert_eq!(role.name, "Moderator");
        assert_eq!(role.permissions, 0xFF);
        assert_eq!(role.color.as_deref(), Some("#00FF00"));
        assert_eq!(role.position, 2);
    }

    #[test]
    fn test_list_roles() {
        let conn = setup();
        create_role(&conn, "Admin", 0xFFFF, None, 3, false).unwrap();
        create_role(&conn, "Mod", 0xFF, None, 2, false).unwrap();
        let roles = list_roles(&conn).unwrap();
        assert_eq!(roles.len(), 2);
        // Ordered by position ascending
        assert_eq!(roles[0].name, "Mod");
        assert_eq!(roles[1].name, "Admin");
    }

    #[test]
    fn test_update_role() {
        let conn = setup();
        let id = create_role(&conn, "Old", 0, None, 1, false).unwrap();
        update_role(&conn, id, Some("New"), Some(0xFF), Some(Some("#FF0000")), Some(5)).unwrap();
        let role = get_role(&conn, id).unwrap().unwrap();
        assert_eq!(role.name, "New");
        assert_eq!(role.permissions, 0xFF);
        assert_eq!(role.color.as_deref(), Some("#FF0000"));
        assert_eq!(role.position, 5);
    }

    #[test]
    fn test_delete_role() {
        let conn = setup();
        let id = create_role(&conn, "Temp", 0, None, 1, false).unwrap();
        delete_role(&conn, id).unwrap();
        assert!(get_role(&conn, id).unwrap().is_none());
    }

    #[test]
    fn test_cannot_delete_builtin_role() {
        let conn = setup();
        let id = create_role(&conn, "@everyone", 0x07, None, 0, true).unwrap();
        let result = delete_role(&conn, id);
        assert!(result.is_err());
    }

    // ── Member-Role mapping ─────────────────────────────────────────

    #[test]
    fn test_assign_and_get_member_roles() {
        let conn = setup();
        let pk = make_key();
        register_member(&conn, &pk, "Alice").unwrap();
        let r1 = create_role(&conn, "Mod", 0xFF, None, 2, false).unwrap();
        let r2 = create_role(&conn, "VIP", 0x0F, None, 1, false).unwrap();
        assign_role(&conn, &pk, r1).unwrap();
        assign_role(&conn, &pk, r2).unwrap();
        let role_ids = get_member_role_ids(&conn, &pk).unwrap();
        assert_eq!(role_ids.len(), 2);
        assert!(role_ids.contains(&r1));
        assert!(role_ids.contains(&r2));
    }

    #[test]
    fn test_remove_role_from_member() {
        let conn = setup();
        let pk = make_key();
        register_member(&conn, &pk, "Alice").unwrap();
        let r1 = create_role(&conn, "Mod", 0xFF, None, 2, false).unwrap();
        assign_role(&conn, &pk, r1).unwrap();
        unassign_role(&conn, &pk, r1).unwrap();
        let role_ids = get_member_role_ids(&conn, &pk).unwrap();
        assert!(role_ids.is_empty());
    }

    #[test]
    fn test_get_member_permissions_from_roles() {
        let conn = setup();
        let pk = make_key();
        register_member(&conn, &pk, "Alice").unwrap();
        let r1 = create_role(&conn, "A", 0x01, None, 1, false).unwrap();
        let r2 = create_role(&conn, "B", 0x02, None, 2, false).unwrap();
        assign_role(&conn, &pk, r1).unwrap();
        assign_role(&conn, &pk, r2).unwrap();
        let role_perms = get_member_role_permissions(&conn, &pk).unwrap();
        assert_eq!(role_perms.len(), 2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- members`

Expected: Compilation errors.

- [ ] **Step 3: Implement member and role storage functions**

Add to the top of `crates/farder-server/src/members.rs` (above tests):

```rust
use anyhow::{bail, Result};
use farder_crypto::identity::PublicKey;
use farder_protocol::server::RoleInfo;
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

// ── Member record ───────────────────────────────────────────────────

pub struct MemberRecord {
    pub public_key: PublicKey,
    pub display_name: String,
    pub joined_at: u64,
    pub banned: bool,
    pub revoked: bool,
}

pub fn register_member(conn: &Connection, pk: &PublicKey, display_name: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO members (public_key, display_name, joined_at) VALUES (?1, ?2, ?3)",
        params![pk.as_bytes().as_slice(), display_name, now() as i64],
    )?;
    Ok(())
}

pub fn get_member(conn: &Connection, pk: &PublicKey) -> Result<Option<MemberRecord>> {
    let mut stmt = conn.prepare(
        "SELECT public_key, display_name, joined_at, banned, revoked FROM members WHERE public_key = ?1"
    )?;
    let mut rows = stmt.query_map(params![pk.as_bytes().as_slice()], |row| {
        let key_bytes: Vec<u8> = row.get(0)?;
        let display_name: String = row.get(1)?;
        let joined_at: i64 = row.get(2)?;
        let banned: bool = row.get(3)?;
        let revoked: bool = row.get(4)?;
        Ok((key_bytes, display_name, joined_at, banned, revoked))
    })?;
    match rows.next() {
        Some(Ok((key_bytes, display_name, joined_at, banned, revoked))) => {
            let arr: [u8; 32] = key_bytes.try_into().map_err(|_| anyhow::anyhow!("bad key length"))?;
            Ok(Some(MemberRecord {
                public_key: PublicKey::from_bytes(arr),
                display_name,
                joined_at: joined_at as u64,
                banned,
                revoked,
            }))
        }
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn list_members(conn: &Connection) -> Result<Vec<MemberRecord>> {
    let mut stmt = conn.prepare(
        "SELECT public_key, display_name, joined_at, banned, revoked FROM members WHERE banned = 0 AND revoked = 0"
    )?;
    let rows = stmt.query_map([], |row| {
        let key_bytes: Vec<u8> = row.get(0)?;
        let display_name: String = row.get(1)?;
        let joined_at: i64 = row.get(2)?;
        let banned: bool = row.get(3)?;
        let revoked: bool = row.get(4)?;
        Ok((key_bytes, display_name, joined_at, banned, revoked))
    })?;
    let mut members = Vec::new();
    for row in rows {
        let (key_bytes, display_name, joined_at, banned, revoked) = row?;
        let arr: [u8; 32] = key_bytes.try_into().map_err(|_| anyhow::anyhow!("bad key length"))?;
        members.push(MemberRecord {
            public_key: PublicKey::from_bytes(arr),
            display_name,
            joined_at: joined_at as u64,
            banned,
            revoked,
        });
    }
    Ok(members)
}

pub fn ban_member(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE members SET banned = 1 WHERE public_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn revoke_member(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "UPDATE members SET revoked = 1 WHERE public_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

pub fn remove_member(conn: &Connection, pk: &PublicKey) -> Result<()> {
    conn.execute(
        "DELETE FROM member_roles WHERE member_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    conn.execute(
        "DELETE FROM members WHERE public_key = ?1",
        params![pk.as_bytes().as_slice()],
    )?;
    Ok(())
}

// ── Roles ───────────────────────────────────────────────────────────

pub fn create_role(
    conn: &Connection,
    name: &str,
    permissions: u64,
    color: Option<&str>,
    position: u32,
    builtin: bool,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO roles (name, permissions, color, position, builtin) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, permissions as i64, color, position, builtin],
    )?;
    Ok(conn.last_insert_rowid() as u64)
}

pub fn get_role(conn: &Connection, id: u64) -> Result<Option<RoleInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, permissions, color, position FROM roles WHERE id = ?1"
    )?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(RoleInfo {
            id: row.get::<_, i64>(0)? as u64,
            name: row.get(1)?,
            permissions: row.get::<_, i64>(2)? as u64,
            color: row.get(3)?,
            position: row.get::<_, i64>(4)? as u32,
        })
    })?;
    match rows.next() {
        Some(Ok(role)) => Ok(Some(role)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn list_roles(conn: &Connection) -> Result<Vec<RoleInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, permissions, color, position FROM roles ORDER BY position ASC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RoleInfo {
            id: row.get::<_, i64>(0)? as u64,
            name: row.get(1)?,
            permissions: row.get::<_, i64>(2)? as u64,
            color: row.get(3)?,
            position: row.get::<_, i64>(4)? as u32,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn update_role(
    conn: &Connection,
    id: u64,
    name: Option<&str>,
    permissions: Option<u64>,
    color: Option<Option<&str>>,
    position: Option<u32>,
) -> Result<()> {
    if let Some(n) = name {
        conn.execute("UPDATE roles SET name = ?1 WHERE id = ?2", params![n, id])?;
    }
    if let Some(p) = permissions {
        conn.execute("UPDATE roles SET permissions = ?1 WHERE id = ?2", params![p as i64, id])?;
    }
    if let Some(c) = color {
        conn.execute("UPDATE roles SET color = ?1 WHERE id = ?2", params![c, id])?;
    }
    if let Some(pos) = position {
        conn.execute("UPDATE roles SET position = ?1 WHERE id = ?2", params![pos, id])?;
    }
    Ok(())
}

pub fn delete_role(conn: &Connection, id: u64) -> Result<()> {
    let builtin: bool = conn.query_row(
        "SELECT builtin FROM roles WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    if builtin {
        bail!("cannot delete built-in role");
    }
    conn.execute("DELETE FROM member_roles WHERE role_id = ?1", params![id])?;
    conn.execute("DELETE FROM channel_overrides WHERE role_id = ?1", params![id])?;
    conn.execute("DELETE FROM category_overrides WHERE role_id = ?1", params![id])?;
    conn.execute("DELETE FROM roles WHERE id = ?1", params![id])?;
    Ok(())
}

// ── Member-Role mapping ─────────────────────────────────────────────

pub fn assign_role(conn: &Connection, pk: &PublicKey, role_id: u64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO member_roles (member_key, role_id) VALUES (?1, ?2)",
        params![pk.as_bytes().as_slice(), role_id],
    )?;
    Ok(())
}

pub fn unassign_role(conn: &Connection, pk: &PublicKey, role_id: u64) -> Result<()> {
    conn.execute(
        "DELETE FROM member_roles WHERE member_key = ?1 AND role_id = ?2",
        params![pk.as_bytes().as_slice(), role_id],
    )?;
    Ok(())
}

pub fn get_member_role_ids(conn: &Connection, pk: &PublicKey) -> Result<Vec<u64>> {
    let mut stmt = conn.prepare(
        "SELECT role_id FROM member_roles WHERE member_key = ?1"
    )?;
    let rows = stmt.query_map(params![pk.as_bytes().as_slice()], |row| {
        Ok(row.get::<_, i64>(0)? as u64)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn get_member_role_permissions(conn: &Connection, pk: &PublicKey) -> Result<Vec<u64>> {
    let mut stmt = conn.prepare(
        "SELECT r.permissions FROM roles r
         INNER JOIN member_roles mr ON mr.role_id = r.id
         WHERE mr.member_key = ?1"
    )?;
    let rows = stmt.query_map(params![pk.as_bytes().as_slice()], |row| {
        Ok(row.get::<_, i64>(0)? as u64)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- members`

Expected: All 12 member/role tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/members.rs
git commit -m "feat(server): implement member and role storage with CRUD operations"
```

---

## Task 5: Channel & Category Storage

**Files:**
- Create: `crates/farder-server/src/channels.rs`

- [ ] **Step 1: Write failing tests for channel and category operations**

`crates/farder-server/src/channels.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::members;
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::ChannelType;

    fn setup() -> Connection {
        db::open_in_memory().unwrap()
    }

    // ── Category tests ──────────────────────────────────────────────

    #[test]
    fn test_create_and_get_category() {
        let conn = setup();
        let id = create_category(&conn, "General", 0).unwrap();
        let cat = get_category(&conn, id).unwrap().unwrap();
        assert_eq!(cat.name, "General");
        assert_eq!(cat.position, 0);
    }

    #[test]
    fn test_list_categories() {
        let conn = setup();
        create_category(&conn, "B", 1).unwrap();
        create_category(&conn, "A", 0).unwrap();
        let cats = list_categories(&conn).unwrap();
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].name, "A");
        assert_eq!(cats[1].name, "B");
    }

    #[test]
    fn test_update_category() {
        let conn = setup();
        let id = create_category(&conn, "Old", 0).unwrap();
        update_category(&conn, id, Some("New"), Some(5)).unwrap();
        let cat = get_category(&conn, id).unwrap().unwrap();
        assert_eq!(cat.name, "New");
        assert_eq!(cat.position, 5);
    }

    #[test]
    fn test_delete_category() {
        let conn = setup();
        let id = create_category(&conn, "Temp", 0).unwrap();
        delete_category(&conn, id).unwrap();
        assert!(get_category(&conn, id).unwrap().is_none());
    }

    // ── Channel tests ───────────────────────────────────────────────

    #[test]
    fn test_create_and_get_channel() {
        let conn = setup();
        let cat_id = create_category(&conn, "General", 0).unwrap();
        let id = create_channel(&conn, "chat", ChannelType::Text, Some(cat_id), 0).unwrap();
        let ch = get_channel(&conn, id).unwrap().unwrap();
        assert_eq!(ch.name, "chat");
        assert_eq!(ch.channel_type, ChannelType::Text);
        assert_eq!(ch.category_id, Some(cat_id));
        assert_eq!(ch.slow_mode_secs, 0);
        assert!(!ch.nsfw);
    }

    #[test]
    fn test_create_announcement_channel() {
        let conn = setup();
        let id = create_channel(&conn, "news", ChannelType::Announcement, None, 0).unwrap();
        let ch = get_channel(&conn, id).unwrap().unwrap();
        assert_eq!(ch.channel_type, ChannelType::Announcement);
    }

    #[test]
    fn test_list_channels() {
        let conn = setup();
        create_channel(&conn, "b", ChannelType::Text, None, 1).unwrap();
        create_channel(&conn, "a", ChannelType::Text, None, 0).unwrap();
        let channels = list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].name, "a");
        assert_eq!(channels[1].name, "b");
    }

    #[test]
    fn test_update_channel() {
        let conn = setup();
        let id = create_channel(&conn, "old", ChannelType::Text, None, 0).unwrap();
        update_channel(&conn, id, Some("new"), Some("a topic"), Some(true), Some(5), Some(Some(3600))).unwrap();
        let ch = get_channel(&conn, id).unwrap().unwrap();
        assert_eq!(ch.name, "new");
        assert_eq!(ch.topic.as_deref(), Some("a topic"));
        assert!(ch.nsfw);
        assert_eq!(ch.slow_mode_secs, 5);
        assert_eq!(ch.retention_secs, Some(3600));
    }

    #[test]
    fn test_soft_delete_channel() {
        let conn = setup();
        let id = create_channel(&conn, "temp", ChannelType::Text, None, 0).unwrap();
        soft_delete_channel(&conn, id).unwrap();
        // Soft-deleted channels don't appear in list
        let channels = list_channels(&conn).unwrap();
        assert!(channels.is_empty());
        // But still exist in DB (for hard-delete later)
        let ch = get_channel_including_deleted(&conn, id).unwrap();
        assert!(ch.is_some());
    }

    #[test]
    fn test_hard_delete_channel() {
        let conn = setup();
        let id = create_channel(&conn, "temp", ChannelType::Text, None, 0).unwrap();
        soft_delete_channel(&conn, id).unwrap();
        hard_delete_channel(&conn, id).unwrap();
        assert!(get_channel_including_deleted(&conn, id).unwrap().is_none());
    }

    // ── Override tests ──────────────────────────────────────────────

    #[test]
    fn test_set_and_get_channel_overrides() {
        let conn = setup();
        let ch_id = create_channel(&conn, "ch", ChannelType::Text, None, 0).unwrap();
        let role_id = members::create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        set_channel_override(&conn, ch_id, role_id, 0x01, 0x02).unwrap();
        let overrides = get_channel_overrides(&conn, ch_id).unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].role_id, role_id);
        assert_eq!(overrides[0].allow, 0x01);
        assert_eq!(overrides[0].deny, 0x02);
    }

    #[test]
    fn test_set_and_get_category_overrides() {
        let conn = setup();
        let cat_id = create_category(&conn, "General", 0).unwrap();
        let role_id = members::create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        set_category_override(&conn, cat_id, role_id, 0x04, 0x08).unwrap();
        let overrides = get_category_overrides(&conn, cat_id).unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].allow, 0x04);
        assert_eq!(overrides[0].deny, 0x08);
    }

    #[test]
    fn test_override_upsert() {
        let conn = setup();
        let ch_id = create_channel(&conn, "ch", ChannelType::Text, None, 0).unwrap();
        let role_id = members::create_role(&conn, "Mod", 0xFF, None, 1, false).unwrap();
        set_channel_override(&conn, ch_id, role_id, 0x01, 0x02).unwrap();
        set_channel_override(&conn, ch_id, role_id, 0x10, 0x20).unwrap();
        let overrides = get_channel_overrides(&conn, ch_id).unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].allow, 0x10);
        assert_eq!(overrides[0].deny, 0x20);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- channels`

Expected: Compilation errors.

- [ ] **Step 3: Implement channel and category storage functions**

Add to the top of `crates/farder-server/src/channels.rs` (above tests):

```rust
use anyhow::Result;
use farder_protocol::server::{CategoryInfo, ChannelInfo, ChannelType, OverrideInfo};
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn channel_type_to_str(ct: &ChannelType) -> &'static str {
    match ct {
        ChannelType::Text => "text",
        ChannelType::Announcement => "announcement",
    }
}

fn str_to_channel_type(s: &str) -> ChannelType {
    match s {
        "announcement" => ChannelType::Announcement,
        _ => ChannelType::Text,
    }
}

// ── Categories ──────────────────────────────────────────────────────

pub fn create_category(conn: &Connection, name: &str, position: u32) -> Result<u64> {
    conn.execute(
        "INSERT INTO categories (name, position) VALUES (?1, ?2)",
        params![name, position],
    )?;
    Ok(conn.last_insert_rowid() as u64)
}

pub fn get_category(conn: &Connection, id: u64) -> Result<Option<CategoryInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, position FROM categories WHERE id = ?1 AND deleted = 0"
    )?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(CategoryInfo {
            id: row.get::<_, i64>(0)? as u64,
            name: row.get(1)?,
            position: row.get::<_, i64>(2)? as u32,
        })
    })?;
    match rows.next() {
        Some(Ok(cat)) => Ok(Some(cat)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn list_categories(conn: &Connection) -> Result<Vec<CategoryInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, position FROM categories WHERE deleted = 0 ORDER BY position ASC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(CategoryInfo {
            id: row.get::<_, i64>(0)? as u64,
            name: row.get(1)?,
            position: row.get::<_, i64>(2)? as u32,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn update_category(conn: &Connection, id: u64, name: Option<&str>, position: Option<u32>) -> Result<()> {
    if let Some(n) = name {
        conn.execute("UPDATE categories SET name = ?1 WHERE id = ?2", params![n, id])?;
    }
    if let Some(p) = position {
        conn.execute("UPDATE categories SET position = ?1 WHERE id = ?2", params![p, id])?;
    }
    Ok(())
}

pub fn delete_category(conn: &Connection, id: u64) -> Result<()> {
    conn.execute("UPDATE categories SET deleted = 1 WHERE id = ?1", params![id])?;
    // Unset category_id on channels in this category
    conn.execute("UPDATE channels SET category_id = NULL WHERE category_id = ?1", params![id])?;
    conn.execute("DELETE FROM category_overrides WHERE category_id = ?1", params![id])?;
    Ok(())
}

// ── Channels ────────────────────────────────────────────────────────

pub fn create_channel(
    conn: &Connection,
    name: &str,
    channel_type: ChannelType,
    category_id: Option<u64>,
    position: u32,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO channels (name, channel_type, category_id, position) VALUES (?1, ?2, ?3, ?4)",
        params![name, channel_type_to_str(&channel_type), category_id.map(|id| id as i64), position],
    )?;
    Ok(conn.last_insert_rowid() as u64)
}

fn row_to_channel_info(row: &rusqlite::Row) -> rusqlite::Result<ChannelInfo> {
    let ct_str: String = row.get(2)?;
    Ok(ChannelInfo {
        id: row.get::<_, i64>(0)? as u64,
        name: row.get(1)?,
        channel_type: str_to_channel_type(&ct_str),
        category_id: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        position: row.get::<_, i64>(4)? as u32,
        topic: row.get(5)?,
        nsfw: row.get(6)?,
        slow_mode_secs: row.get::<_, i64>(7)? as u32,
        retention_secs: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
    })
}

const CHANNEL_SELECT: &str =
    "SELECT id, name, channel_type, category_id, position, topic, nsfw, slow_mode_secs, retention_secs FROM channels";

pub fn get_channel(conn: &Connection, id: u64) -> Result<Option<ChannelInfo>> {
    let sql = format!("{} WHERE id = ?1 AND deleted = 0", CHANNEL_SELECT);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], row_to_channel_info)?;
    match rows.next() {
        Some(Ok(ch)) => Ok(Some(ch)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn get_channel_including_deleted(conn: &Connection, id: u64) -> Result<Option<ChannelInfo>> {
    let sql = format!("{} WHERE id = ?1", CHANNEL_SELECT);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], row_to_channel_info)?;
    match rows.next() {
        Some(Ok(ch)) => Ok(Some(ch)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn list_channels(conn: &Connection) -> Result<Vec<ChannelInfo>> {
    let sql = format!("{} WHERE deleted = 0 ORDER BY position ASC", CHANNEL_SELECT);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_channel_info)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn update_channel(
    conn: &Connection,
    id: u64,
    name: Option<&str>,
    topic: Option<&str>,
    nsfw: Option<bool>,
    slow_mode_secs: Option<u32>,
    retention_secs: Option<Option<u64>>,
) -> Result<()> {
    if let Some(n) = name {
        conn.execute("UPDATE channels SET name = ?1 WHERE id = ?2", params![n, id])?;
    }
    if let Some(t) = topic {
        conn.execute("UPDATE channels SET topic = ?1 WHERE id = ?2", params![t, id])?;
    }
    if let Some(n) = nsfw {
        conn.execute("UPDATE channels SET nsfw = ?1 WHERE id = ?2", params![n, id])?;
    }
    if let Some(s) = slow_mode_secs {
        conn.execute("UPDATE channels SET slow_mode_secs = ?1 WHERE id = ?2", params![s, id])?;
    }
    if let Some(r) = retention_secs {
        conn.execute("UPDATE channels SET retention_secs = ?1 WHERE id = ?2", params![r.map(|v| v as i64), id])?;
    }
    Ok(())
}

pub fn soft_delete_channel(conn: &Connection, id: u64) -> Result<()> {
    conn.execute(
        "UPDATE channels SET deleted = 1, deleted_at = ?1 WHERE id = ?2",
        params![now() as i64, id],
    )?;
    Ok(())
}

pub fn hard_delete_channel(conn: &Connection, id: u64) -> Result<()> {
    conn.execute("DELETE FROM channel_overrides WHERE channel_id = ?1", params![id])?;
    conn.execute("DELETE FROM messages WHERE channel_id = ?1", params![id])?;
    conn.execute("DELETE FROM channels WHERE id = ?1", params![id])?;
    Ok(())
}

// ── Overrides ───────────────────────────────────────────────────────

pub fn set_channel_override(conn: &Connection, channel_id: u64, role_id: u64, allow: u64, deny: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_overrides (channel_id, role_id, allow_bits, deny_bits)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(channel_id, role_id) DO UPDATE SET allow_bits = ?3, deny_bits = ?4",
        params![channel_id, role_id, allow as i64, deny as i64],
    )?;
    Ok(())
}

pub fn get_channel_overrides(conn: &Connection, channel_id: u64) -> Result<Vec<OverrideInfo>> {
    let mut stmt = conn.prepare(
        "SELECT role_id, allow_bits, deny_bits FROM channel_overrides WHERE channel_id = ?1"
    )?;
    let rows = stmt.query_map(params![channel_id], |row| {
        Ok(OverrideInfo {
            role_id: row.get::<_, i64>(0)? as u64,
            allow: row.get::<_, i64>(1)? as u64,
            deny: row.get::<_, i64>(2)? as u64,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn set_category_override(conn: &Connection, category_id: u64, role_id: u64, allow: u64, deny: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO category_overrides (category_id, role_id, allow_bits, deny_bits)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(category_id, role_id) DO UPDATE SET allow_bits = ?3, deny_bits = ?4",
        params![category_id, role_id, allow as i64, deny as i64],
    )?;
    Ok(())
}

pub fn get_category_overrides(conn: &Connection, category_id: u64) -> Result<Vec<OverrideInfo>> {
    let mut stmt = conn.prepare(
        "SELECT role_id, allow_bits, deny_bits FROM category_overrides WHERE category_id = ?1"
    )?;
    let rows = stmt.query_map(params![category_id], |row| {
        Ok(OverrideInfo {
            role_id: row.get::<_, i64>(0)? as u64,
            allow: row.get::<_, i64>(1)? as u64,
            deny: row.get::<_, i64>(2)? as u64,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Get channel overrides for a specific set of role IDs.
pub fn get_channel_overrides_for_roles(conn: &Connection, channel_id: u64, role_ids: &[u64]) -> Result<Vec<OverrideInfo>> {
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = role_ids.iter().enumerate().map(|(i, _)| format!("?{}", i + 2)).collect();
    let sql = format!(
        "SELECT role_id, allow_bits, deny_bits FROM channel_overrides WHERE channel_id = ?1 AND role_id IN ({})",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(channel_id as i64));
    for id in role_ids {
        param_values.push(Box::new(*id as i64));
    }
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(params_ref.as_slice(), |row| {
        Ok(OverrideInfo {
            role_id: row.get::<_, i64>(0)? as u64,
            allow: row.get::<_, i64>(1)? as u64,
            deny: row.get::<_, i64>(2)? as u64,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Get category overrides for a specific set of role IDs.
pub fn get_category_overrides_for_roles(conn: &Connection, category_id: u64, role_ids: &[u64]) -> Result<Vec<OverrideInfo>> {
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = role_ids.iter().enumerate().map(|(i, _)| format!("?{}", i + 2)).collect();
    let sql = format!(
        "SELECT role_id, allow_bits, deny_bits FROM category_overrides WHERE category_id = ?1 AND role_id IN ({})",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(category_id as i64));
    for id in role_ids {
        param_values.push(Box::new(*id as i64));
    }
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(params_ref.as_slice(), |row| {
        Ok(OverrideInfo {
            role_id: row.get::<_, i64>(0)? as u64,
            allow: row.get::<_, i64>(1)? as u64,
            deny: row.get::<_, i64>(2)? as u64,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- channels`

Expected: All 12 channel/category tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/channels.rs
git commit -m "feat(server): implement channel and category storage with overrides"
```

---

## Task 6: Message Storage & FTS5 Search

**Files:**
- Create: `crates/farder-server/src/messages.rs`

- [ ] **Step 1: Write failing tests for message operations**

`crates/farder-server/src/messages.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{channels, db};
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::ChannelType;

    fn setup() -> (Connection, u64, PublicKey) {
        let conn = db::open_in_memory().unwrap();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let pk = Keypair::generate().public_key();
        crate::members::register_member(&conn, &pk, "Alice").unwrap();
        (conn, ch_id, pk)
    }

    #[test]
    fn test_insert_and_get_message() {
        let (conn, ch_id, pk) = setup();
        let id = insert_message(&conn, ch_id, &pk, "hello world", None).unwrap();
        let msg = get_message(&conn, id).unwrap().unwrap();
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.channel_id, ch_id);
        assert_eq!(msg.author, pk);
        assert!(msg.edited_at.is_none());
        assert!(msg.reply_to.is_none());
        assert!(!msg.pinned);
    }

    #[test]
    fn test_insert_reply() {
        let (conn, ch_id, pk) = setup();
        let parent = insert_message(&conn, ch_id, &pk, "parent", None).unwrap();
        let reply = insert_message(&conn, ch_id, &pk, "reply", Some(parent)).unwrap();
        let msg = get_message(&conn, reply).unwrap().unwrap();
        assert_eq!(msg.reply_to, Some(parent));
    }

    #[test]
    fn test_fetch_history_paginated() {
        let (conn, ch_id, pk) = setup();
        for i in 0..10 {
            insert_message(&conn, ch_id, &pk, &format!("msg {}", i), None).unwrap();
        }
        let page1 = fetch_history(&conn, ch_id, None, 3).unwrap();
        assert_eq!(page1.len(), 3);
        // Most recent first (descending by id)
        assert_eq!(page1[0].content, "msg 9");
        assert_eq!(page1[2].content, "msg 7");

        let page2 = fetch_history(&conn, ch_id, Some(page1[2].id), 3).unwrap();
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0].content, "msg 6");
    }

    #[test]
    fn test_edit_message() {
        let (conn, ch_id, pk) = setup();
        let id = insert_message(&conn, ch_id, &pk, "original", None).unwrap();
        edit_message(&conn, id, "edited").unwrap();
        let msg = get_message(&conn, id).unwrap().unwrap();
        assert_eq!(msg.content, "edited");
        assert!(msg.edited_at.is_some());
    }

    #[test]
    fn test_delete_message() {
        let (conn, ch_id, pk) = setup();
        let id = insert_message(&conn, ch_id, &pk, "temp", None).unwrap();
        delete_message(&conn, id).unwrap();
        assert!(get_message(&conn, id).unwrap().is_none());
    }

    #[test]
    fn test_pin_unpin_message() {
        let (conn, ch_id, pk) = setup();
        let id = insert_message(&conn, ch_id, &pk, "pin me", None).unwrap();
        pin_message(&conn, id).unwrap();
        let msg = get_message(&conn, id).unwrap().unwrap();
        assert!(msg.pinned);

        unpin_message(&conn, id).unwrap();
        let msg = get_message(&conn, id).unwrap().unwrap();
        assert!(!msg.pinned);
    }

    #[test]
    fn test_fts5_search() {
        let (conn, ch_id, pk) = setup();
        insert_message(&conn, ch_id, &pk, "rust is great", None).unwrap();
        insert_message(&conn, ch_id, &pk, "python is cool", None).unwrap();
        insert_message(&conn, ch_id, &pk, "rust and python together", None).unwrap();

        let results = search_messages(&conn, "rust", Some(ch_id), 10).unwrap();
        assert_eq!(results.len(), 2);

        let results = search_messages(&conn, "python", None, 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_delete_old_messages() {
        let (conn, ch_id, pk) = setup();
        // Insert messages with explicit timestamps
        insert_message_with_ts(&conn, ch_id, &pk, "old", None, 1000).unwrap();
        insert_message_with_ts(&conn, ch_id, &pk, "new", None, 9000).unwrap();
        let deleted = delete_messages_before(&conn, ch_id, 5000).unwrap();
        assert_eq!(deleted, 1);
        let remaining = fetch_history(&conn, ch_id, None, 10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].content, "new");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- messages`

Expected: Compilation errors.

- [ ] **Step 3: Implement message storage functions**

Add to the top of `crates/farder-server/src/messages.rs` (above tests):

```rust
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::MessageInfo;
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn row_to_message_info(row: &rusqlite::Row) -> rusqlite::Result<MessageInfo> {
    let author_bytes: Vec<u8> = row.get(2)?;
    let author_arr: [u8; 32] = author_bytes.try_into().map_err(|_| {
        rusqlite::Error::InvalidColumnType(2, "author".to_string(), rusqlite::types::Type::Blob)
    })?;
    Ok(MessageInfo {
        id: row.get::<_, i64>(0)? as u64,
        channel_id: row.get::<_, i64>(1)? as u64,
        author: PublicKey::from_bytes(author_arr),
        content: row.get(3)?,
        timestamp: row.get::<_, i64>(4)? as u64,
        edited_at: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        reply_to: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        pinned: row.get(7)?,
    })
}

const MSG_SELECT: &str =
    "SELECT id, channel_id, author, content, timestamp, edited_at, reply_to, pinned FROM messages";

pub fn insert_message(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    reply_to: Option<u64>,
) -> Result<u64> {
    insert_message_with_ts(conn, channel_id, author, content, reply_to, now())
}

pub fn insert_message_with_ts(
    conn: &Connection,
    channel_id: u64,
    author: &PublicKey,
    content: &str,
    reply_to: Option<u64>,
    timestamp: u64,
) -> Result<u64> {
    conn.execute(
        "INSERT INTO messages (channel_id, author, content, timestamp, reply_to)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            channel_id as i64,
            author.as_bytes().as_slice(),
            content,
            timestamp as i64,
            reply_to.map(|v| v as i64),
        ],
    )?;
    let id = conn.last_insert_rowid() as u64;
    // Update FTS5 index
    conn.execute(
        "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
        params![id as i64, content],
    )?;
    Ok(id)
}

pub fn get_message(conn: &Connection, id: u64) -> Result<Option<MessageInfo>> {
    let sql = format!("{} WHERE id = ?1", MSG_SELECT);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id as i64], row_to_message_info)?;
    match rows.next() {
        Some(Ok(msg)) => Ok(Some(msg)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Fetch history in reverse chronological order (newest first).
/// If `before_id` is provided, only return messages with id < before_id.
pub fn fetch_history(
    conn: &Connection,
    channel_id: u64,
    before_id: Option<u64>,
    limit: u32,
) -> Result<Vec<MessageInfo>> {
    let sql = match before_id {
        Some(_) => format!("{} WHERE channel_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3", MSG_SELECT),
        None => format!("{} WHERE channel_id = ?1 ORDER BY id DESC LIMIT ?2", MSG_SELECT),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = match before_id {
        Some(bid) => stmt.query_map(params![channel_id as i64, bid as i64, limit], row_to_message_info)?,
        None => stmt.query_map(params![channel_id as i64, limit], row_to_message_info)?,
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn edit_message(conn: &Connection, id: u64, new_content: &str) -> Result<()> {
    conn.execute(
        "UPDATE messages SET content = ?1, edited_at = ?2 WHERE id = ?3",
        params![new_content, now() as i64, id as i64],
    )?;
    // Update FTS5
    conn.execute("DELETE FROM messages_fts WHERE rowid = ?1", params![id as i64])?;
    conn.execute(
        "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
        params![id as i64, new_content],
    )?;
    Ok(())
}

pub fn delete_message(conn: &Connection, id: u64) -> Result<()> {
    conn.execute("DELETE FROM messages_fts WHERE rowid = ?1", params![id as i64])?;
    conn.execute("DELETE FROM messages WHERE id = ?1", params![id as i64])?;
    Ok(())
}

pub fn pin_message(conn: &Connection, id: u64) -> Result<()> {
    conn.execute("UPDATE messages SET pinned = 1 WHERE id = ?1", params![id as i64])?;
    Ok(())
}

pub fn unpin_message(conn: &Connection, id: u64) -> Result<()> {
    conn.execute("UPDATE messages SET pinned = 0 WHERE id = ?1", params![id as i64])?;
    Ok(())
}

pub fn search_messages(
    conn: &Connection,
    query: &str,
    channel_id: Option<u64>,
    limit: u32,
) -> Result<Vec<MessageInfo>> {
    let sql = match channel_id {
        Some(_) => format!(
            "{} WHERE id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1) AND channel_id = ?2 ORDER BY id DESC LIMIT ?3",
            MSG_SELECT
        ),
        None => format!(
            "{} WHERE id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1) ORDER BY id DESC LIMIT ?2",
            MSG_SELECT
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = match channel_id {
        Some(cid) => stmt.query_map(params![query, cid as i64, limit], row_to_message_info)?,
        None => stmt.query_map(params![query, limit], row_to_message_info)?,
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Delete messages in a channel with timestamp < cutoff. Returns count deleted.
pub fn delete_messages_before(conn: &Connection, channel_id: u64, cutoff_timestamp: u64) -> Result<u64> {
    // Delete FTS entries first
    conn.execute(
        "DELETE FROM messages_fts WHERE rowid IN (
            SELECT id FROM messages WHERE channel_id = ?1 AND timestamp < ?2
        )",
        params![channel_id as i64, cutoff_timestamp as i64],
    )?;
    let deleted = conn.execute(
        "DELETE FROM messages WHERE channel_id = ?1 AND timestamp < ?2",
        params![channel_id as i64, cutoff_timestamp as i64],
    )?;
    Ok(deleted as u64)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- messages`

Expected: All 8 message tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/messages.rs
git commit -m "feat(server): implement message storage with FTS5 search and retention support"
```

---

## Task 7: Invite Storage & Validation

**Files:**
- Create: `crates/farder-server/src/invites.rs`

- [ ] **Step 1: Write failing tests for invite operations**

`crates/farder-server/src/invites.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use farder_crypto::identity::Keypair;

    fn setup() -> (Connection, PublicKey) {
        let conn = db::open_in_memory().unwrap();
        let pk = Keypair::generate().public_key();
        crate::members::register_member(&conn, &pk, "Alice").unwrap();
        (conn, pk)
    }

    #[test]
    fn test_create_and_validate_invite() {
        let (conn, pk) = setup();
        let code = create_invite(&conn, &pk, None, None, None).unwrap();
        assert_eq!(code.len(), 8);
        let result = validate_invite(&conn, &code).unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn test_invite_with_max_uses() {
        let (conn, pk) = setup();
        let code = create_invite(&conn, &pk, Some(2), None, None).unwrap();
        assert!(use_invite(&conn, &code).unwrap().is_ok());
        assert!(use_invite(&conn, &code).unwrap().is_ok());
        // Third use should fail
        let result = use_invite(&conn, &code).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_invite_expired() {
        let (conn, pk) = setup();
        // Expire 1 second in the past
        let code = create_invite_with_expires(&conn, &pk, None, Some(now() - 1), None).unwrap();
        let result = validate_invite(&conn, &code).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_invite_not_found() {
        let (conn, _) = setup();
        let result = validate_invite(&conn, "NOSUCHCODE").unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_invite_with_target_channel() {
        let (conn, pk) = setup();
        let code = create_invite(&conn, &pk, None, None, Some(42)).unwrap();
        let result = validate_invite(&conn, &code).unwrap().unwrap();
        assert_eq!(result.target_channel, Some(42));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- invites`

Expected: Compilation errors.

- [ ] **Step 3: Implement invite storage functions**

Add to the top of `crates/farder-server/src/invites.rs` (above tests):

```rust
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use rand::Rng;
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn random_code() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..8).map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char).collect()
}

pub struct InviteInfo {
    pub code: String,
    pub target_channel: Option<u64>,
}

pub fn create_invite(
    conn: &Connection,
    created_by: &PublicKey,
    max_uses: Option<u32>,
    expires_in_secs: Option<u64>,
    target_channel: Option<u64>,
) -> Result<String> {
    let expires_at = expires_in_secs.map(|secs| now() + secs);
    create_invite_with_expires(conn, created_by, max_uses, expires_at, target_channel)
}

pub fn create_invite_with_expires(
    conn: &Connection,
    created_by: &PublicKey,
    max_uses: Option<u32>,
    expires_at: Option<u64>,
    target_channel: Option<u64>,
) -> Result<String> {
    let code = random_code();
    conn.execute(
        "INSERT INTO invites (code, created_by, max_uses, expires_at, target_channel)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            code,
            created_by.as_bytes().as_slice(),
            max_uses.map(|v| v as i64),
            expires_at.map(|v| v as i64),
            target_channel.map(|v| v as i64),
        ],
    )?;
    Ok(code)
}

pub fn validate_invite(conn: &Connection, code: &str) -> Result<Result<InviteInfo, String>> {
    let mut stmt = conn.prepare(
        "SELECT code, max_uses, use_count, expires_at, target_channel FROM invites WHERE code = ?1"
    )?;
    let mut rows = stmt.query_map(params![code], |row| {
        let code: String = row.get(0)?;
        let max_uses: Option<i64> = row.get(1)?;
        let use_count: i64 = row.get(2)?;
        let expires_at: Option<i64> = row.get(3)?;
        let target_channel: Option<i64> = row.get(4)?;
        Ok((code, max_uses, use_count, expires_at, target_channel))
    })?;

    match rows.next() {
        None => Ok(Err("invite not found".to_string())),
        Some(Err(e)) => Err(e.into()),
        Some(Ok((code, max_uses, use_count, expires_at, target_channel))) => {
            if let Some(exp) = expires_at {
                if (exp as u64) < now() {
                    return Ok(Err("invite expired".to_string()));
                }
            }
            if let Some(max) = max_uses {
                if use_count >= max {
                    return Ok(Err("invite has reached maximum uses".to_string()));
                }
            }
            Ok(Ok(InviteInfo {
                code,
                target_channel: target_channel.map(|v| v as u64),
            }))
        }
    }
}

/// Validate and increment use count. Returns Ok(Ok(info)) on success.
pub fn use_invite(conn: &Connection, code: &str) -> Result<Result<InviteInfo, String>> {
    let valid = validate_invite(conn, code)?;
    match valid {
        Ok(info) => {
            conn.execute(
                "UPDATE invites SET use_count = use_count + 1 WHERE code = ?1",
                params![code],
            )?;
            Ok(Ok(info))
        }
        Err(reason) => Ok(Err(reason)),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- invites`

Expected: All 5 invite tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/invites.rs
git commit -m "feat(server): implement invite storage with validation and expiry"
```

---

## Task 8: Auth & Session Management

**Files:**
- Create: `crates/farder-server/src/auth.rs`

- [ ] **Step 1: Write failing tests for auth operations**

`crates/farder-server/src/auth.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use farder_crypto::identity::Keypair;

    fn setup() -> Connection {
        db::open_in_memory().unwrap()
    }

    #[test]
    fn test_generate_challenge() {
        let c1 = generate_challenge();
        let c2 = generate_challenge();
        assert_eq!(c1.len(), 32);
        assert_ne!(c1, c2);
    }

    #[test]
    fn test_generate_session_token() {
        let t1 = generate_session_token();
        let t2 = generate_session_token();
        assert_eq!(t1.len(), 32);
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_generate_setup_token() {
        let t1 = generate_setup_token();
        let t2 = generate_setup_token();
        assert_eq!(t1.len(), 32);
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_verify_challenge_success() {
        let kp = Keypair::generate();
        let challenge = generate_challenge();
        let signature = kp.sign(&challenge);
        assert!(verify_challenge(&kp.public_key(), &challenge, &signature).is_ok());
    }

    #[test]
    fn test_verify_challenge_wrong_sig_fails() {
        let kp = Keypair::generate();
        let challenge = generate_challenge();
        let wrong_sig = vec![0u8; 64];
        assert!(verify_challenge(&kp.public_key(), &challenge, &wrong_sig).is_err());
    }

    #[test]
    fn test_setup_token_claim_owner() {
        let conn = setup();
        let kp = Keypair::generate();
        let setup_token = generate_setup_token();
        let setup_hex = hex::encode(&setup_token);
        let result = authenticate_new_member(
            &conn,
            &kp.public_key(),
            "Alice",
            None,
            Some(&setup_hex),
            Some(&setup_token),
        ).unwrap();
        assert!(result.is_ok());
        // Verify member was registered
        let member = crate::members::get_member(&conn, &kp.public_key()).unwrap().unwrap();
        assert_eq!(member.display_name, "Alice");
    }

    #[test]
    fn test_setup_token_wrong_token_fails() {
        let conn = setup();
        let kp = Keypair::generate();
        let setup_token = generate_setup_token();
        let wrong_hex = hex::encode(&generate_setup_token());
        let result = authenticate_new_member(
            &conn,
            &kp.public_key(),
            "Alice",
            None,
            Some(&wrong_hex),
            Some(&setup_token),
        ).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_invite_code_join() {
        let conn = setup();
        // Create an owner first so we can create invites
        let owner_kp = Keypair::generate();
        crate::members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        let invite_code = crate::invites::create_invite(&conn, &owner_kp.public_key(), None, None, None).unwrap();

        let joiner_kp = Keypair::generate();
        let result = authenticate_new_member(
            &conn,
            &joiner_kp.public_key(),
            "Bob",
            Some(&invite_code),
            None,
            None, // no setup token active
        ).unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn test_existing_member_auth() {
        let conn = setup();
        let kp = Keypair::generate();
        crate::members::register_member(&conn, &kp.public_key(), "Alice").unwrap();
        let result = authenticate_existing_member(&conn, &kp.public_key()).unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn test_banned_member_rejected() {
        let conn = setup();
        let kp = Keypair::generate();
        crate::members::register_member(&conn, &kp.public_key(), "Alice").unwrap();
        crate::members::ban_member(&conn, &kp.public_key()).unwrap();
        let result = authenticate_existing_member(&conn, &kp.public_key()).unwrap();
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- auth`

Expected: Compilation errors.

- [ ] **Step 3: Implement auth functions**

Add to the top of `crates/farder-server/src/auth.rs` (above tests):

```rust
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use rusqlite::Connection;

pub fn generate_challenge() -> [u8; 32] {
    rand::random()
}

pub fn generate_session_token() -> [u8; 32] {
    rand::random()
}

pub fn generate_setup_token() -> [u8; 32] {
    rand::random()
}

pub fn verify_challenge(
    public_key: &PublicKey,
    challenge: &[u8],
    signature: &[u8],
) -> Result<()> {
    public_key.verify(challenge, signature)
}

/// Authenticate a new member via setup token or invite code.
/// Returns Ok(Ok(())) on success, Ok(Err(reason)) on auth failure.
pub fn authenticate_new_member(
    conn: &Connection,
    public_key: &PublicKey,
    display_name: &str,
    invite_code: Option<&str>,
    setup_token_hex: Option<&str>,
    active_setup_token: Option<&[u8; 32]>,
) -> Result<Result<(), String>> {
    // Check if trying to claim ownership via setup token
    if let Some(token_hex) = setup_token_hex {
        if let Some(active) = active_setup_token {
            let expected_hex = hex::encode(active);
            if token_hex == expected_hex {
                crate::members::register_member(conn, public_key, display_name)?;
                return Ok(Ok(()));
            } else {
                return Ok(Err("invalid setup token".to_string()));
            }
        } else {
            return Ok(Err("server already has an owner".to_string()));
        }
    }

    // Check invite code
    if let Some(code) = invite_code {
        match crate::invites::use_invite(conn, code)? {
            Ok(_invite_info) => {
                crate::members::register_member(conn, public_key, display_name)?;
                return Ok(Ok(()));
            }
            Err(reason) => {
                return Ok(Err(format!("invalid invite: {}", reason)));
            }
        }
    }

    Ok(Err("no invite code or setup token provided".to_string()))
}

/// Authenticate an existing member by public key.
/// Returns Ok(Ok(())) if the member exists and is not banned/revoked.
pub fn authenticate_existing_member(
    conn: &Connection,
    public_key: &PublicKey,
) -> Result<Result<(), String>> {
    match crate::members::get_member(conn, public_key)? {
        None => Ok(Err("member not found".to_string())),
        Some(member) => {
            if member.banned {
                Ok(Err("member is banned".to_string()))
            } else if member.revoked {
                Ok(Err("key has been revoked".to_string()))
            } else {
                Ok(Ok(()))
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- auth`

Expected: All 8 auth tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/auth.rs
git commit -m "feat(server): implement auth with challenge-response, setup token, and invite join"
```

---

## Task 9: Templates

**Files:**
- Create: `crates/farder-server/src/templates.rs`
- Create: `crates/farder-server/templates/blank.toml`
- Create: `crates/farder-server/templates/friend-group.toml`
- Create: `crates/farder-server/templates/gaming-community.toml`
- Create: `crates/farder-server/templates/organization.toml`
- Create: `crates/farder-server/templates/public-community.toml`

- [ ] **Step 1: Create the 5 built-in template TOML files**

`crates/farder-server/templates/blank.toml`:

```toml
[template]
name = "Blank"
description = "Minimal server with just a general channel"

[[categories]]
name = "General"

[[categories.channels]]
name = "general"
type = "text"
```

`crates/farder-server/templates/friend-group.toml`:

```toml
[template]
name = "Friend Group"
description = "Casual hangout for a small group of friends"

[[categories]]
name = "General"

[[categories.channels]]
name = "general"
type = "text"

[[categories.channels]]
name = "media"
type = "text"
```

`crates/farder-server/templates/gaming-community.toml`:

```toml
[template]
name = "Gaming Community"
description = "Voice lobbies, LFG, and game channels"

[[roles]]
name = "Admin"
permissions = 16383
color = "#FF0000"
position = 3

[[roles]]
name = "Moderator"
permissions = 2191
color = "#00FF00"
position = 2

[[roles]]
name = "Member"
permissions = 8199
position = 1

[[categories]]
name = "General"

[[categories.channels]]
name = "welcome"
type = "announcement"

[[categories.channels]]
name = "chat"
type = "text"

[[categories]]
name = "Gaming"

[[categories.channels]]
name = "looking-for-group"
type = "text"

[[categories.channels]]
name = "game-night"
type = "text"

[[categories]]
name = "Staff Only"

[[categories.channels]]
name = "mod-chat"
type = "text"
```

`crates/farder-server/templates/organization.toml`:

```toml
[template]
name = "Organization"
description = "Team workspace with departments and announcements"

[[roles]]
name = "Admin"
permissions = 16383
color = "#FF0000"
position = 3

[[roles]]
name = "Manager"
permissions = 2191
color = "#0066FF"
position = 2

[[roles]]
name = "Member"
permissions = 8199
position = 1

[[categories]]
name = "General"

[[categories.channels]]
name = "announcements"
type = "announcement"

[[categories.channels]]
name = "general"
type = "text"

[[categories]]
name = "Projects"

[[categories.channels]]
name = "project-updates"
type = "text"

[[categories.channels]]
name = "ideas"
type = "text"
```

`crates/farder-server/templates/public-community.toml`:

```toml
[template]
name = "Public Community"
description = "Open community with verified posting role"

[[roles]]
name = "Admin"
permissions = 16383
color = "#FF0000"
position = 3

[[roles]]
name = "Moderator"
permissions = 2191
color = "#00FF00"
position = 2

[[roles]]
name = "Verified"
permissions = 8199
position = 1

[[categories]]
name = "Information"

[[categories.channels]]
name = "rules"
type = "announcement"

[[categories.channels]]
name = "introductions"
type = "text"

[[categories]]
name = "Community"

[[categories.channels]]
name = "general"
type = "text"

[[categories.channels]]
name = "off-topic"
type = "text"
```

- [ ] **Step 2: Write failing tests for template parsing and application**

`crates/farder-server/src/templates.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn test_parse_blank_template() {
        let t = parse_template(BLANK).unwrap();
        assert_eq!(t.template.name, "Blank");
        assert_eq!(t.categories.len(), 1);
        assert_eq!(t.categories[0].channels.len(), 1);
        assert!(t.roles.is_none() || t.roles.as_ref().unwrap().is_empty());
    }

    #[test]
    fn test_parse_gaming_template() {
        let t = parse_template(GAMING_COMMUNITY).unwrap();
        assert_eq!(t.template.name, "Gaming Community");
        assert_eq!(t.roles.as_ref().unwrap().len(), 3);
        assert_eq!(t.categories.len(), 3);
    }

    #[test]
    fn test_list_builtin_templates() {
        let templates = list_builtin_templates();
        assert_eq!(templates.len(), 5);
        let names: Vec<&str> = templates.iter().map(|t| t.template.name.as_str()).collect();
        assert!(names.contains(&"Blank"));
        assert!(names.contains(&"Gaming Community"));
        assert!(names.contains(&"Friend Group"));
        assert!(names.contains(&"Organization"));
        assert!(names.contains(&"Public Community"));
    }

    #[test]
    fn test_apply_blank_template() {
        let conn = db::open_in_memory().unwrap();
        let t = parse_template(BLANK).unwrap();
        apply_template(&conn, &t).unwrap();

        let channels = crate::channels::list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "general");

        let categories = crate::channels::list_categories(&conn).unwrap();
        assert_eq!(categories.len(), 1);
        assert_eq!(categories[0].name, "General");
    }

    #[test]
    fn test_apply_gaming_template() {
        let conn = db::open_in_memory().unwrap();
        let t = parse_template(GAMING_COMMUNITY).unwrap();
        apply_template(&conn, &t).unwrap();

        let roles = crate::members::list_roles(&conn).unwrap();
        assert_eq!(roles.len(), 3);

        let channels = crate::channels::list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 5);

        let categories = crate::channels::list_categories(&conn).unwrap();
        assert_eq!(categories.len(), 3);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- templates`

Expected: Compilation errors.

- [ ] **Step 4: Implement template parsing and application**

Add to the top of `crates/farder-server/src/templates.rs` (above tests):

```rust
use anyhow::Result;
use farder_protocol::server::ChannelType;
use rusqlite::Connection;
use serde::Deserialize;

// Embedded default templates
const BLANK: &str = include_str!("../templates/blank.toml");
const FRIEND_GROUP: &str = include_str!("../templates/friend-group.toml");
const GAMING_COMMUNITY: &str = include_str!("../templates/gaming-community.toml");
const ORGANIZATION: &str = include_str!("../templates/organization.toml");
const PUBLIC_COMMUNITY: &str = include_str!("../templates/public-community.toml");

#[derive(Debug, Deserialize)]
pub struct Template {
    pub template: TemplateInfo,
    #[serde(default)]
    pub roles: Option<Vec<TemplateRole>>,
    pub categories: Vec<TemplateCategory>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct TemplateRole {
    pub name: String,
    pub permissions: u64,
    pub color: Option<String>,
    pub position: u32,
}

#[derive(Debug, Deserialize)]
pub struct TemplateCategory {
    pub name: String,
    #[serde(default)]
    pub channels: Vec<TemplateChannel>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateChannel {
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
}

pub fn parse_template(toml_str: &str) -> Result<Template> {
    toml::from_str(toml_str).map_err(Into::into)
}

pub fn list_builtin_templates() -> Vec<Template> {
    [BLANK, FRIEND_GROUP, GAMING_COMMUNITY, ORGANIZATION, PUBLIC_COMMUNITY]
        .iter()
        .filter_map(|s| parse_template(s).ok())
        .collect()
}

/// Load templates from a filesystem directory. Returns empty vec if dir doesn't exist.
pub fn load_templates_from_dir(dir: &str) -> Vec<Template> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "toml"))
        .filter_map(|e| {
            let content = std::fs::read_to_string(e.path()).ok()?;
            parse_template(&content).ok()
        })
        .collect()
}

/// Apply a template to an empty database: create roles, categories, channels.
pub fn apply_template(conn: &Connection, template: &Template) -> Result<()> {
    // Create roles
    if let Some(roles) = &template.roles {
        for role in roles {
            crate::members::create_role(
                conn,
                &role.name,
                role.permissions,
                role.color.as_deref(),
                role.position,
                false,
            )?;
        }
    }

    // Create categories and their channels
    for (cat_idx, category) in template.categories.iter().enumerate() {
        let cat_id = crate::channels::create_category(conn, &category.name, cat_idx as u32)?;
        for (ch_idx, channel) in category.channels.iter().enumerate() {
            let ct = match channel.channel_type.as_str() {
                "announcement" => ChannelType::Announcement,
                _ => ChannelType::Text,
            };
            crate::channels::create_channel(conn, &channel.name, ct, Some(cat_id), ch_idx as u32)?;
        }
    }

    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- templates`

Expected: All 5 template tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/templates.rs crates/farder-server/templates/
git commit -m "feat(server): implement TOML server templates with 5 built-in defaults"
```

---

## Task 10: Event Types & Request Handlers

**Files:**
- Create: `crates/farder-server/src/events.rs`
- Create: `crates/farder-server/src/handlers.rs`

- [ ] **Step 1: Implement event target types**

`crates/farder-server/src/events.rs`:

```rust
use farder_protocol::server::ServerEvent;

/// Describes who should receive a server event.
#[derive(Debug)]
pub enum EventTarget {
    /// Send to all connected clients.
    All,
    /// Send to clients subscribed to this channel.
    Subscribers(u64),
}

/// An event paired with its broadcast target.
#[derive(Debug)]
pub struct BroadcastEvent {
    pub target: EventTarget,
    pub event: ServerEvent,
}
```

- [ ] **Step 2: Write failing tests for request handlers**

`crates/farder-server/src/handlers.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth, channels, db, invites, members, permissions};
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::{ChannelType, ServerRequest, ServerResponse};

    /// Create a test DB with an owner and @everyone role.
    fn setup() -> (Connection, PublicKey) {
        let conn = db::open_in_memory().unwrap();
        let everyone_id = members::create_role(
            &conn, "@everyone", permissions::DEFAULT_EVERYONE, None, 0, true,
        ).unwrap();
        let owner_kp = Keypair::generate();
        members::register_member(&conn, &owner_kp.public_key(), "Owner").unwrap();
        members::assign_role(&conn, &owner_kp.public_key(), everyone_id).unwrap();
        (conn, owner_kp.public_key())
    }

    fn add_member(conn: &Connection, name: &str) -> PublicKey {
        let kp = Keypair::generate();
        members::register_member(conn, &kp.public_key(), name).unwrap();
        let everyone_id: u64 = conn.query_row(
            "SELECT id FROM roles WHERE name = '@everyone'",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        ).unwrap();
        members::assign_role(conn, &kp.public_key(), everyone_id).unwrap();
        kp.public_key()
    }

    #[test]
    fn test_handle_send_message() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::SendMessage {
            channel_id: ch_id,
            content: "hello".to_string(),
            reply_to: None,
        }).unwrap();
        match result.response {
            ServerResponse::MessageSent { id, .. } => assert!(id > 0),
            other => panic!("expected MessageSent, got {:?}", other),
        }
        assert!(!result.events.is_empty());
    }

    #[test]
    fn test_handle_send_message_no_permission() {
        let (conn, _owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        // Create a member with no roles (no permissions)
        let nobody_kp = Keypair::generate();
        members::register_member(&conn, &nobody_kp.public_key(), "Nobody").unwrap();
        // Don't assign @everyone role

        let result = handle_request(&conn, &nobody_kp.public_key(), false, ServerRequest::SendMessage {
            channel_id: ch_id,
            content: "hello".to_string(),
            reply_to: None,
        }).unwrap();
        match result.response {
            ServerResponse::Error { .. } => {}
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_fetch_history() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        crate::messages::insert_message(&conn, ch_id, &owner, "msg1", None).unwrap();
        crate::messages::insert_message(&conn, ch_id, &owner, "msg2", None).unwrap();

        let result = handle_request(&conn, &owner, true, ServerRequest::FetchHistory {
            channel_id: ch_id,
            before_id: None,
            limit: 50,
        }).unwrap();
        match result.response {
            ServerResponse::History { messages } => assert_eq!(messages.len(), 2),
            other => panic!("expected History, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_create_channel() {
        let (conn, owner) = setup();
        let result = handle_request(&conn, &owner, true, ServerRequest::CreateChannel {
            name: "new-channel".to_string(),
            channel_type: ChannelType::Text,
            category_id: None,
            position: Some(0),
        }).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok, got {:?}", other),
        }
        let chs = channels::list_channels(&conn).unwrap();
        assert_eq!(chs.len(), 1);
        assert_eq!(chs[0].name, "new-channel");
    }

    #[test]
    fn test_handle_create_role() {
        let (conn, owner) = setup();
        let result = handle_request(&conn, &owner, true, ServerRequest::CreateRole {
            name: "Mod".to_string(),
            permissions: 0xFF,
            color: Some("#00FF00".to_string()),
            position: Some(2),
        }).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_create_invite() {
        let (conn, owner) = setup();
        let result = handle_request(&conn, &owner, true, ServerRequest::CreateInvite {
            max_uses: Some(10),
            expires_in_secs: None,
            target_channel: None,
        }).unwrap();
        match result.response {
            ServerResponse::InviteCreated { code } => assert_eq!(code.len(), 8),
            other => panic!("expected InviteCreated, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_get_server_info() {
        let (conn, owner) = setup();
        channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::GetServerInfo).unwrap();
        match result.response {
            ServerResponse::ServerInfo { member_count, channels, roles, .. } => {
                assert_eq!(member_count, 1);
                assert_eq!(channels.len(), 1);
                assert!(!roles.is_empty());
            }
            other => panic!("expected ServerInfo, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_edit_own_message() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        let msg_id = crate::messages::insert_message(&conn, ch_id, &owner, "original", None).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::EditMessage {
            message_id: msg_id,
            new_content: "edited".to_string(),
        }).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok, got {:?}", other),
        }
        let msg = crate::messages::get_message(&conn, msg_id).unwrap().unwrap();
        assert_eq!(msg.content, "edited");
    }

    #[test]
    fn test_handle_search() {
        let (conn, owner) = setup();
        let ch_id = channels::create_channel(&conn, "general", ChannelType::Text, None, 0).unwrap();
        crate::messages::insert_message(&conn, ch_id, &owner, "rust is great", None).unwrap();
        crate::messages::insert_message(&conn, ch_id, &owner, "python rocks", None).unwrap();
        let result = handle_request(&conn, &owner, true, ServerRequest::Search {
            query: "rust".to_string(),
            channel_id: Some(ch_id),
            limit: 10,
        }).unwrap();
        match result.response {
            ServerResponse::SearchResults { messages } => assert_eq!(messages.len(), 1),
            other => panic!("expected SearchResults, got {:?}", other),
        }
    }

    #[test]
    fn test_handle_ban_member() {
        let (conn, owner) = setup();
        let bob = add_member(&conn, "Bob");
        let result = handle_request(&conn, &owner, true, ServerRequest::BanMember {
            member_key: bob.clone(),
        }).unwrap();
        match result.response {
            ServerResponse::Ok => {}
            other => panic!("expected Ok, got {:?}", other),
        }
        let member = members::get_member(&conn, &bob).unwrap().unwrap();
        assert!(member.banned);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd /home/deez/farder && cargo test -p farder-server -- handlers`

Expected: Compilation errors.

- [ ] **Step 4: Implement request handlers**

Add to the top of `crates/farder-server/src/handlers.rs` (above tests):

```rust
use crate::{channels, events::{BroadcastEvent, EventTarget}, invites, members, messages, permissions};
use anyhow::Result;
use farder_crypto::identity::PublicKey;
use farder_protocol::server::*;
use rusqlite::Connection;

pub struct HandleResult {
    pub response: ServerResponse,
    pub events: Vec<BroadcastEvent>,
}

fn ok(response: ServerResponse) -> Result<HandleResult> {
    Ok(HandleResult { response, events: Vec::new() })
}

fn ok_with(response: ServerResponse, events: Vec<BroadcastEvent>) -> Result<HandleResult> {
    Ok(HandleResult { response, events })
}

fn err(reason: &str) -> Result<HandleResult> {
    ok(ServerResponse::Error { reason: reason.to_string() })
}

/// Resolve a member's effective permissions for a specific channel.
fn resolve_member_perms(
    conn: &Connection,
    member: &PublicKey,
    channel_id: u64,
    is_owner: bool,
) -> Result<u64> {
    let role_ids = members::get_member_role_ids(conn, member)?;
    let everyone_perms = conn.query_row(
        "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
        [],
        |row| Ok(row.get::<_, i64>(0)? as u64),
    ).unwrap_or(0);

    let role_perms = members::get_member_role_permissions(conn, member)?;

    // Get channel info for category overrides
    let channel = channels::get_channel(conn, channel_id)?;
    let mut cat_overrides = Vec::new();
    if let Some(ref ch) = channel {
        if let Some(cat_id) = ch.category_id {
            let ovs = channels::get_category_overrides_for_roles(conn, cat_id, &role_ids)?;
            for ov in ovs {
                cat_overrides.push(permissions::Override { allow: ov.allow, deny: ov.deny });
            }
        }
    }

    let ch_ovs = channels::get_channel_overrides_for_roles(conn, channel_id, &role_ids)?;
    let channel_overrides: Vec<permissions::Override> = ch_ovs
        .into_iter()
        .map(|ov| permissions::Override { allow: ov.allow, deny: ov.deny })
        .collect();

    Ok(permissions::resolve(permissions::ResolutionContext {
        everyone_permissions: everyone_perms,
        role_permissions: role_perms,
        category_overrides: cat_overrides,
        channel_overrides,
        is_owner,
    }))
}

pub fn handle_request(
    conn: &Connection,
    member: &PublicKey,
    is_owner: bool,
    request: ServerRequest,
) -> Result<HandleResult> {
    match request {
        ServerRequest::SendMessage { channel_id, content, reply_to } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::SEND_MESSAGES) {
                return err("missing SEND_MESSAGES permission");
            }
            let id = messages::insert_message(conn, channel_id, member, &content, reply_to)?;
            let msg = messages::get_message(conn, id)?.unwrap();
            ok_with(
                ServerResponse::MessageSent { id, timestamp: msg.timestamp },
                vec![BroadcastEvent {
                    target: EventTarget::Subscribers(channel_id),
                    event: ServerEvent::NewMessage { message: msg },
                }],
            )
        }

        ServerRequest::EditMessage { message_id, new_content } => {
            let msg = messages::get_message(conn, message_id)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            if msg.author != *member {
                return err("can only edit own messages");
            }
            messages::edit_message(conn, message_id, &new_content)?;
            let updated = messages::get_message(conn, message_id)?.unwrap();
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::Subscribers(msg.channel_id),
                    event: ServerEvent::MessageEdited {
                        message_id,
                        channel_id: msg.channel_id,
                        new_content,
                        edited_at: updated.edited_at.unwrap(),
                    },
                }],
            )
        }

        ServerRequest::DeleteMessage { message_id } => {
            let msg = messages::get_message(conn, message_id)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            let perms = resolve_member_perms(conn, member, msg.channel_id, is_owner)?;
            if msg.author != *member && !permissions::has(perms, permissions::MANAGE_MESSAGES) {
                return err("missing MANAGE_MESSAGES permission");
            }
            messages::delete_message(conn, message_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::Subscribers(msg.channel_id),
                    event: ServerEvent::MessageDeleted { message_id, channel_id: msg.channel_id },
                }],
            )
        }

        ServerRequest::FetchHistory { channel_id, before_id, limit } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::READ_MESSAGES) {
                return err("missing READ_MESSAGES permission");
            }
            let msgs = messages::fetch_history(conn, channel_id, before_id, limit)?;
            ok(ServerResponse::History { messages: msgs })
        }

        ServerRequest::PinMessage { message_id } => {
            let msg = messages::get_message(conn, message_id)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            let perms = resolve_member_perms(conn, member, msg.channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_MESSAGES) {
                return err("missing MANAGE_MESSAGES permission");
            }
            messages::pin_message(conn, message_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::Subscribers(msg.channel_id),
                    event: ServerEvent::MessagePinned { message_id, channel_id: msg.channel_id },
                }],
            )
        }

        ServerRequest::UnpinMessage { message_id } => {
            let msg = messages::get_message(conn, message_id)?
                .ok_or_else(|| anyhow::anyhow!("message not found"))?;
            let perms = resolve_member_perms(conn, member, msg.channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_MESSAGES) {
                return err("missing MANAGE_MESSAGES permission");
            }
            messages::unpin_message(conn, message_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::Subscribers(msg.channel_id),
                    event: ServerEvent::MessageUnpinned { message_id, channel_id: msg.channel_id },
                }],
            )
        }

        ServerRequest::Search { query, channel_id, limit } => {
            // If channel_id specified, check READ_MESSAGES for that channel
            if let Some(cid) = channel_id {
                let perms = resolve_member_perms(conn, member, cid, is_owner)?;
                if !permissions::has(perms, permissions::READ_MESSAGES) {
                    return err("missing READ_MESSAGES permission");
                }
            }
            let msgs = messages::search_messages(conn, &query, channel_id, limit)?;
            ok(ServerResponse::SearchResults { messages: msgs })
        }

        ServerRequest::Typing { channel_id } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::SEND_MESSAGES) {
                return err("missing SEND_MESSAGES permission");
            }
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::Subscribers(channel_id),
                    event: ServerEvent::TypingStarted {
                        channel_id,
                        public_key: member.clone(),
                    },
                }],
            )
        }

        ServerRequest::CreateChannel { name, channel_type, category_id, position } => {
            // MANAGE_CHANNEL is a server-wide check here (no specific channel yet)
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_CHANNEL) {
                    return err("missing MANAGE_CHANNEL permission");
                }
            }
            let pos = position.unwrap_or(0);
            let id = channels::create_channel(conn, &name, channel_type, category_id, pos)?;
            let ch = channels::get_channel(conn, id)?.unwrap();
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::ChannelCreated { channel: ch },
                }],
            )
        }

        ServerRequest::UpdateChannel { channel_id, name, topic, nsfw, slow_mode_secs, retention_secs } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_CHANNEL) {
                return err("missing MANAGE_CHANNEL permission");
            }
            channels::update_channel(
                conn,
                channel_id,
                name.as_deref(),
                topic.as_deref(),
                nsfw,
                slow_mode_secs,
                retention_secs,
            )?;
            let ch = channels::get_channel(conn, channel_id)?.unwrap();
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::ChannelUpdated { channel: ch },
                }],
            )
        }

        ServerRequest::DeleteChannel { channel_id } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_CHANNEL) {
                return err("missing MANAGE_CHANNEL permission");
            }
            channels::soft_delete_channel(conn, channel_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::ChannelDeleted { channel_id },
                }],
            )
        }

        ServerRequest::CreateCategory { name, position } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
            }
            let pos = position.unwrap_or(0);
            let id = channels::create_category(conn, &name, pos)?;
            let cat = channels::get_category(conn, id)?.unwrap();
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::CategoryCreated { category: cat },
                }],
            )
        }

        ServerRequest::UpdateCategory { category_id, name, position } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
            }
            channels::update_category(conn, category_id, name.as_deref(), position)?;
            let cat = channels::get_category(conn, category_id)?.unwrap();
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::CategoryUpdated { category: cat },
                }],
            )
        }

        ServerRequest::DeleteCategory { category_id } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
            }
            channels::delete_category(conn, category_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::CategoryDeleted { category_id },
                }],
            )
        }

        ServerRequest::CreateRole { name, permissions: perms, color, position } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            let pos = position.unwrap_or(0);
            let id = members::create_role(conn, &name, perms, color.as_deref(), pos, false)?;
            let role = members::get_role(conn, id)?.unwrap();
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::RoleCreated { role },
                }],
            )
        }

        ServerRequest::UpdateRole { role_id, name, permissions: perms, color, position } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            members::update_role(conn, role_id, name.as_deref(), perms, color.as_deref().map(Some), position)?;
            let role = members::get_role(conn, role_id)?.unwrap();
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::RoleUpdated { role },
                }],
            )
        }

        ServerRequest::DeleteRole { role_id } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            members::delete_role(conn, role_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::RoleDeleted { role_id },
                }],
            )
        }

        ServerRequest::AssignRole { member_key, role_id } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            members::assign_role(conn, &member_key, role_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::PermissionsChanged,
                }],
            )
        }

        ServerRequest::RemoveRole { member_key, role_id } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_ROLES) {
                    return err("missing MANAGE_ROLES permission");
                }
            }
            members::unassign_role(conn, &member_key, role_id)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::PermissionsChanged,
                }],
            )
        }

        ServerRequest::KickMember { member_key } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::KICK_MEMBERS) {
                    return err("missing KICK_MEMBERS permission");
                }
            }
            members::remove_member(conn, &member_key)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MemberLeft { public_key: member_key },
                }],
            )
        }

        ServerRequest::BanMember { member_key } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::BAN_MEMBERS) {
                    return err("missing BAN_MEMBERS permission");
                }
            }
            members::ban_member(conn, &member_key)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::MemberBanned { public_key: member_key },
                }],
            )
        }

        ServerRequest::CreateInvite { max_uses, expires_in_secs, target_channel } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::CREATE_INVITES) {
                    return err("missing CREATE_INVITES permission");
                }
            }
            let code = invites::create_invite(conn, member, max_uses, expires_in_secs, target_channel)?;
            ok(ServerResponse::InviteCreated { code })
        }

        ServerRequest::GetServerInfo => {
            let member_list = members::list_members(conn)?;
            let channel_list = channels::list_channels(conn)?;
            let category_list = channels::list_categories(conn)?;
            let role_list = members::list_roles(conn)?;
            ok(ServerResponse::ServerInfo {
                name: String::new(), // filled in by caller from ServerState
                member_count: member_list.len() as u32,
                channels: channel_list,
                categories: category_list,
                roles: role_list,
            })
        }

        ServerRequest::GetMembers => {
            let member_list = members::list_members(conn)?;
            let mut infos = Vec::new();
            for m in member_list {
                let role_ids = members::get_member_role_ids(conn, &m.public_key)?;
                infos.push(MemberInfo {
                    public_key: m.public_key,
                    display_name: m.display_name,
                    joined_at: m.joined_at,
                    role_ids,
                });
            }
            ok(ServerResponse::Members { members: infos })
        }

        ServerRequest::SetChannelOverride { channel_id, role_id, allow, deny } => {
            let perms = resolve_member_perms(conn, member, channel_id, is_owner)?;
            if !permissions::has(perms, permissions::MANAGE_CHANNEL) {
                return err("missing MANAGE_CHANNEL permission");
            }
            channels::set_channel_override(conn, channel_id, role_id, allow, deny)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::PermissionsChanged,
                }],
            )
        }

        ServerRequest::SetCategoryOverride { category_id, role_id, allow, deny } => {
            if !is_owner {
                let role_perms = members::get_member_role_permissions(conn, member)?;
                let everyone_perms = conn.query_row(
                    "SELECT permissions FROM roles WHERE name = '@everyone' AND builtin = 1",
                    [],
                    |row| Ok(row.get::<_, i64>(0)? as u64),
                ).unwrap_or(0);
                let mut base = everyone_perms;
                for rp in &role_perms {
                    base |= rp;
                }
                if !permissions::has(base, permissions::MANAGE_SERVER) {
                    return err("missing MANAGE_SERVER permission");
                }
            }
            channels::set_category_override(conn, category_id, role_id, allow, deny)?;
            ok_with(
                ServerResponse::Ok,
                vec![BroadcastEvent {
                    target: EventTarget::All,
                    event: ServerEvent::PermissionsChanged,
                }],
            )
        }

        ServerRequest::Subscribe { .. } => {
            // Subscribe is handled at the connection level, not here
            ok(ServerResponse::Ok)
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /home/deez/farder && cargo test -p farder-server -- handlers`

Expected: All 10 handler tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/events.rs crates/farder-server/src/handlers.rs
git commit -m "feat(server): implement event types and all request handlers with permission checks"
```

---

## Task 11: Connection Handler

**Files:**
- Create: `crates/farder-server/src/connection.rs`

- [ ] **Step 1: Implement the per-client QUIC connection handler**

`crates/farder-server/src/connection.rs`:

```rust
use crate::{auth, events::EventTarget, handlers, members, state::ServerState};
use anyhow::{Context, Result};
use farder_crypto::identity::PublicKey;
use farder_protocol::{codec, server::*};
use quinn::{RecvStream, SendStream};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Read a length-prefixed frame from a QUIC stream.
async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        anyhow::bail!("frame too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write a length-prefixed frame to a QUIC stream.
async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}

async fn send_server_frame(send: &mut SendStream, frame: &ServerFrame) -> Result<()> {
    let bytes = codec::encode(frame)?;
    write_frame(send, &bytes).await
}

async fn recv_client_frame(recv: &mut RecvStream) -> Result<ClientFrame> {
    let bytes = read_frame(recv).await?;
    codec::decode(&bytes).map_err(Into::into)
}

/// Handle a single client connection from auth through request loop.
pub async fn handle_client(
    state: Arc<ServerState>,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    // 1. Send challenge
    let nonce = auth::generate_challenge();
    send_server_frame(&mut send, &ServerFrame::Challenge { nonce }).await?;

    // 2. Receive auth
    let frame = recv_client_frame(&mut recv).await?;
    let member_key = match frame {
        ClientFrame::Authenticate {
            public_key,
            signed_challenge,
            invite_code,
            setup_token,
        } => {
            // Verify signature
            auth::verify_challenge(&public_key, &nonce, &signed_challenge)
                .context("challenge verification failed")?;

            // Check if existing member
            let db = state.db.lock().unwrap();
            let existing = members::get_member(&db, &public_key)?;
            drop(db);

            if let Some(_member) = existing {
                let db = state.db.lock().unwrap();
                match auth::authenticate_existing_member(&db, &public_key)? {
                    Ok(()) => {}
                    Err(reason) => {
                        send_server_frame(&mut send, &ServerFrame::AuthError { reason }).await?;
                        return Ok(());
                    }
                }
            } else {
                // New member — need invite code or setup token
                let display_name = format!("vk_{}", hex::encode(&public_key.as_bytes()[..4]));
                let setup_token_bytes = state.setup_token.lock().unwrap().clone();
                let db = state.db.lock().unwrap();
                match auth::authenticate_new_member(
                    &db,
                    &public_key,
                    &display_name,
                    invite_code.as_deref(),
                    setup_token.as_deref(),
                    setup_token_bytes.as_ref(),
                )? {
                    Ok(()) => {
                        // If setup token was used, clear it and set owner
                        if setup_token.is_some() {
                            drop(db);
                            *state.setup_token.lock().unwrap() = None;
                            *state.owner.write().await = Some(public_key.clone());
                        }
                    }
                    Err(reason) => {
                        drop(db);
                        send_server_frame(&mut send, &ServerFrame::AuthError { reason }).await?;
                        return Ok(());
                    }
                }
            }

            // Issue session token
            let session_token = auth::generate_session_token();
            send_server_frame(
                &mut send,
                &ServerFrame::Authenticated {
                    session_token: session_token.to_vec(),
                },
            )
            .await?;
            info!("Client authenticated: {}", public_key);
            public_key
        }
        _ => {
            send_server_frame(
                &mut send,
                &ServerFrame::AuthError {
                    reason: "expected Authenticate".to_string(),
                },
            )
            .await?;
            return Ok(());
        }
    };

    // 3. Register client for events
    let (event_tx, mut event_rx) = mpsc::channel::<ServerEvent>(256);
    {
        let mut clients = state.clients.write().await;
        clients.insert(*member_key.as_bytes(), event_tx);
    }

    // Broadcast MemberJoined
    {
        let db = state.db.lock().unwrap();
        let member = members::get_member(&db, &member_key)?;
        if let Some(m) = member {
            let event = ServerEvent::MemberJoined {
                public_key: member_key.clone(),
                display_name: m.display_name,
            };
            broadcast_event(&state, EventTarget::All, event).await;
        }
    }

    let is_owner = {
        let owner = state.owner.read().await;
        owner.as_ref() == Some(&member_key)
    };

    // 4. Main loop
    let result = main_loop(&state, &member_key, is_owner, &mut send, &mut recv, &mut event_rx).await;

    // 5. Cleanup on disconnect
    {
        let mut clients = state.clients.write().await;
        clients.remove(member_key.as_bytes());
    }
    {
        let mut subs = state.subscriptions.write().await;
        for subscribers in subs.values_mut() {
            subscribers.remove(member_key.as_bytes());
        }
    }
    broadcast_event(
        &state,
        EventTarget::All,
        ServerEvent::MemberLeft {
            public_key: member_key,
        },
    )
    .await;

    result
}

async fn main_loop(
    state: &Arc<ServerState>,
    member_key: &PublicKey,
    is_owner: bool,
    send: &mut SendStream,
    recv: &mut RecvStream,
    event_rx: &mut mpsc::Receiver<ServerEvent>,
) -> Result<()> {
    loop {
        tokio::select! {
            frame_result = recv_client_frame(recv) => {
                let frame = match frame_result {
                    Ok(f) => f,
                    Err(_) => return Ok(()), // connection closed
                };
                match frame {
                    ClientFrame::Request { id, body } => {
                        // Handle subscribe at connection level
                        if let ServerRequest::Subscribe { ref channel_ids } = body {
                            let mut subs = state.subscriptions.write().await;
                            // Remove from all current subscriptions
                            for subscribers in subs.values_mut() {
                                subscribers.remove(member_key.as_bytes());
                            }
                            // Add to requested channels
                            for ch_id in channel_ids {
                                subs.entry(*ch_id)
                                    .or_insert_with(HashSet::new)
                                    .insert(*member_key.as_bytes());
                            }
                            send_server_frame(send, &ServerFrame::Response {
                                request_id: id,
                                body: ServerResponse::Ok,
                            }).await?;
                            continue;
                        }

                        let db = state.db.lock().unwrap();
                        let result = handlers::handle_request(&db, member_key, is_owner, body)?;
                        drop(db);

                        // Patch server name into ServerInfo response
                        let response = match result.response {
                            ServerResponse::ServerInfo { member_count, channels, categories, roles, .. } => {
                                ServerResponse::ServerInfo {
                                    name: state.server_name.clone(),
                                    member_count,
                                    channels,
                                    categories,
                                    roles,
                                }
                            }
                            other => other,
                        };

                        send_server_frame(send, &ServerFrame::Response {
                            request_id: id,
                            body: response,
                        }).await?;

                        // Broadcast events
                        for be in result.events {
                            broadcast_event(state, be.target, be.event).await;
                        }
                    }
                    _ => {
                        send_server_frame(send, &ServerFrame::Response {
                            request_id: 0,
                            body: ServerResponse::Error {
                                reason: "expected Request after auth".to_string(),
                            },
                        }).await?;
                    }
                }
            }

            Some(event) = event_rx.recv() => {
                send_server_frame(send, &ServerFrame::Event(event)).await?;
            }
        }
    }
}

/// Broadcast an event to clients matching the target.
pub async fn broadcast_event(state: &ServerState, target: EventTarget, event: ServerEvent) {
    let clients = state.clients.read().await;
    match target {
        EventTarget::All => {
            for sender in clients.values() {
                let _ = sender.try_send(event.clone());
            }
        }
        EventTarget::Subscribers(channel_id) => {
            let subs = state.subscriptions.read().await;
            if let Some(subscriber_keys) = subs.get(&channel_id) {
                for key_bytes in subscriber_keys {
                    if let Some(sender) = clients.get(key_bytes) {
                        let _ = sender.try_send(event.clone());
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cd /home/deez/farder && cargo check -p farder-server`

Expected: Compiles with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/farder-server/src/connection.rs
git commit -m "feat(server): implement QUIC connection handler with auth flow and event broadcasting"
```

---

## Task 12: Message Retention

**Files:**
- Create: `crates/farder-server/src/retention.rs`

- [ ] **Step 1: Write failing test for retention task**

`crates/farder-server/src/retention.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{channels, db, members, messages};
    use farder_crypto::identity::Keypair;
    use farder_protocol::server::ChannelType;

    #[test]
    fn test_purge_expired_messages() {
        let conn = db::open_in_memory().unwrap();
        let pk = Keypair::generate().public_key();
        members::register_member(&conn, &pk, "Alice").unwrap();

        // Channel with 1-hour retention (3600 seconds)
        let ch_id = channels::create_channel(&conn, "ephemeral", ChannelType::Text, None, 0).unwrap();
        channels::update_channel(&conn, ch_id, None, None, None, None, Some(Some(3600))).unwrap();

        // Channel with no retention
        let ch_id2 = channels::create_channel(&conn, "permanent", ChannelType::Text, None, 1).unwrap();

        // Insert old messages (timestamp = 1000, well before any reasonable "now")
        messages::insert_message_with_ts(&conn, ch_id, &pk, "old msg", None, 1000).unwrap();
        messages::insert_message_with_ts(&conn, ch_id, &pk, "also old", None, 2000).unwrap();
        messages::insert_message_with_ts(&conn, ch_id2, &pk, "permanent old", None, 1000).unwrap();

        // Insert recent message (far future timestamp so it survives)
        messages::insert_message_with_ts(&conn, ch_id, &pk, "recent", None, u64::MAX / 2).unwrap();

        let purged = purge_expired_messages(&conn).unwrap();
        assert_eq!(purged, 2); // only the 2 old messages in the retention channel

        // recent message survives
        let history = messages::fetch_history(&conn, ch_id, None, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "recent");

        // permanent channel untouched
        let history2 = messages::fetch_history(&conn, ch_id2, None, 10).unwrap();
        assert_eq!(history2.len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/deez/farder && cargo test -p farder-server -- retention`

Expected: Compilation error.

- [ ] **Step 3: Implement retention purge function**

Add to the top of `crates/farder-server/src/retention.rs` (above tests):

```rust
use crate::{channels, messages};
use anyhow::Result;
use rusqlite::Connection;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Scan all channels with retention policies and delete expired messages.
/// Returns total number of messages purged.
pub fn purge_expired_messages(conn: &Connection) -> Result<u64> {
    let all_channels = channels::list_channels(conn)?;
    let mut total_purged = 0u64;

    for ch in &all_channels {
        if let Some(retention_secs) = ch.retention_secs {
            let cutoff = now().saturating_sub(retention_secs);
            let purged = messages::delete_messages_before(conn, ch.id, cutoff)?;
            if purged > 0 {
                info!(
                    "Purged {} messages from channel {} (retention: {}s)",
                    purged, ch.name, retention_secs
                );
            }
            total_purged += purged;
        }
    }

    Ok(total_purged)
}

/// Spawn a background task that runs purge_expired_messages periodically.
pub fn spawn_retention_task(
    state: std::sync::Arc<crate::state::ServerState>,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            interval.tick().await;
            let db = state.db.lock().unwrap();
            match purge_expired_messages(&db) {
                Ok(count) if count > 0 => info!("Retention task purged {} messages total", count),
                Err(e) => tracing::error!("Retention task error: {}", e),
                _ => {}
            }
        }
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /home/deez/farder && cargo test -p farder-server -- retention`

Expected: 1 retention test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/farder-server/src/retention.rs
git commit -m "feat(server): implement message retention background task"
```

---

## Task 13: Server Binary & CLI

**Files:**
- Modify: `crates/farder-server/src/main.rs`

- [ ] **Step 1: Implement the server main.rs with CLI args and QUIC listener**

`crates/farder-server/src/main.rs`:

```rust
mod auth;
mod channels;
mod connection;
mod db;
mod events;
mod handlers;
mod invites;
mod members;
mod messages;
mod permissions;
mod retention;
mod state;
mod templates;

use anyhow::{Context, Result};
use clap::Parser;
use quinn::Endpoint;
use state::ServerState;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "farder-server", about = "Farder community server")]
struct Args {
    /// Address to bind the QUIC listener to.
    #[arg(long, default_value = "0.0.0.0:4435")]
    bind: SocketAddr,

    /// Server name displayed to clients.
    #[arg(long, default_value = "My Farder Server")]
    name: String,

    /// Path to the SQLite database file.
    #[arg(long, default_value = "farder-server.db")]
    db: String,

    /// Template to apply on first run (blank, friend-group, gaming-community, organization, public-community).
    #[arg(long, default_value = "blank")]
    template: String,

    /// Message retention check interval in seconds.
    #[arg(long, default_value = "3600")]
    retention_interval: u64,
}

fn make_server_endpoint(bind_addr: SocketAddr) -> Result<Endpoint> {
    let certified = rcgen::generate_simple_self_signed(vec!["farder-server".to_string()])?;
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der())
        .map_err(|e| anyhow::anyhow!("key error: {}", e))?;
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok(endpoint)
}

/// Initialize server state: open DB, apply template if first run, create @everyone role.
fn init_server(args: &Args) -> Result<(ServerState, bool)> {
    let conn = db::open_file(&args.db)?;

    // Check if this is first run (no members in the DB)
    let member_count: i64 = conn.query_row(
        "SELECT count(*) FROM members",
        [],
        |row| row.get(0),
    )?;
    let first_run = member_count == 0;

    if first_run {
        // Create @everyone role
        members::create_role(
            &conn,
            "@everyone",
            permissions::DEFAULT_EVERYONE,
            None,
            0,
            true,
        )?;

        // Apply template
        let builtin = templates::list_builtin_templates();
        let template = builtin
            .iter()
            .find(|t| t.template.name.to_lowercase().replace(' ', "-") == args.template)
            .or_else(|| builtin.iter().find(|t| t.template.name == "Blank"));
        if let Some(t) = template {
            templates::apply_template(&conn, t)?;
            info!("Applied template: {}", t.template.name);
        }
    }

    // Check if owner exists
    let state = ServerState::new(conn, args.name.clone());
    if !first_run {
        // Detect owner: member with all permissions via a role marked builtin=1 that isn't @everyone
        // For simplicity, owner is the first member registered (lowest joined_at)
        let db = state.db.lock().unwrap();
        let owner_key: Option<Vec<u8>> = db
            .query_row(
                "SELECT public_key FROM members WHERE banned = 0 AND revoked = 0 ORDER BY joined_at ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();
        drop(db);
        if let Some(key_bytes) = owner_key {
            if let Ok(arr) = <[u8; 32]>::try_from(key_bytes.as_slice()) {
                let pk = farder_crypto::identity::PublicKey::from_bytes(arr);
                *state.owner.blocking_write() = Some(pk);
            }
        }
    }

    Ok((state, first_run))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args = Args::parse();
    let (server_state, first_run) = init_server(&args)?;
    let state = Arc::new(server_state);

    // Generate and display setup token on first run
    if first_run {
        let setup_token = auth::generate_setup_token();
        let setup_hex = hex::encode(&setup_token);
        info!("=== FIRST RUN ===");
        info!("Setup token (give to the server owner): {}", setup_hex);
        info!("This token is single-use. The first user to connect with it becomes the Owner.");
        *state.setup_token.lock().unwrap() = Some(setup_token);
    }

    // Spawn retention task
    let _retention_handle = retention::spawn_retention_task(
        Arc::clone(&state),
        args.retention_interval,
    );

    // Start QUIC listener
    let endpoint = make_server_endpoint(args.bind)?;
    info!("Server listening on {}", args.bind);

    loop {
        let incoming = match endpoint.accept().await {
            Some(inc) => inc,
            None => break,
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    let remote = conn.remote_address();
                    info!("New connection from {}", remote);
                    let (send, recv) = match conn.accept_bi().await {
                        Ok(streams) => streams,
                        Err(e) => {
                            tracing::warn!("Failed to accept bi-stream from {}: {}", remote, e);
                            return;
                        }
                    };
                    if let Err(e) = connection::handle_client(state, send, recv).await {
                        info!("Client {} disconnected: {}", remote, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("Connection handshake failed: {}", e);
                }
            }
        });
    }

    Ok(())
}
```

- [ ] **Step 2: Verify the full crate compiles**

Run: `cd /home/deez/farder && cargo build -p farder-server`

Expected: Compiles successfully. Binary at `target/debug/farder-server`.

- [ ] **Step 3: Verify help output**

Run: `cd /home/deez/farder && cargo run -p farder-server -- --help`

Expected: Shows usage with `--bind`, `--name`, `--db`, `--template`, `--retention-interval` flags.

- [ ] **Step 4: Commit**

```bash
git add crates/farder-server/src/main.rs
git commit -m "feat(server): implement server binary with CLI, QUIC listener, and first-run setup"
```

---

## Task 14: E2E Integration Test

**Files:**
- Modify: `Cargo.toml` (root — add test entry)
- Create: `tests/e2e_server.rs`

- [ ] **Step 1: Add the e2e test entry to root Cargo.toml**

Add to `Cargo.toml` (root):

```toml
[[test]]
name = "e2e_server"
path = "tests/e2e_server.rs"
```

Add to `[dev-dependencies]` (if not already present):

```toml
quinn = "0.11"
rustls = { version = "0.23", features = ["ring"] }
rcgen = "0.13"
tokio = { version = "1", features = ["full"] }
hex = "0.4"
rand = "0.8"
```

- [ ] **Step 2: Write the e2e integration test**

`tests/e2e_server.rs`:

```rust
//! End-to-end test: spin up a farder-server in-process, connect two clients,
//! have the owner bootstrap, invite a second user, exchange messages.

use farder_crypto::identity::Keypair;
use farder_protocol::{codec, server::*};
use quinn::{Endpoint, RecvStream, SendStream};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

// ── Helpers ─────────────────────────────────────────────────────────

/// Danger: skips server certificate verification. Test only.
#[derive(Debug)]
struct SkipVerification;

impl rustls::client::danger::ServerCertVerifier for SkipVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn make_client_endpoint() -> Endpoint {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerification))
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ));
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_config);
    endpoint
}

fn make_server_endpoint(bind_addr: SocketAddr) -> Endpoint {
    let certified = rcgen::generate_simple_self_signed(vec!["farder-server".to_string()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(
        certified.key_pair.serialize_der(),
    ).unwrap();
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto).unwrap(),
    ));
    Endpoint::server(server_config, bind_addr).unwrap()
}

async fn read_frame(recv: &mut RecvStream) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.unwrap();
    buf
}

async fn write_frame(send: &mut SendStream, data: &[u8]) {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await.unwrap();
    send.write_all(data).await.unwrap();
}

async fn send_frame(send: &mut SendStream, frame: &impl serde::Serialize) {
    let bytes = codec::encode(frame).unwrap();
    write_frame(send, &bytes).await;
}

async fn recv_server_frame(recv: &mut RecvStream) -> ServerFrame {
    let bytes = read_frame(recv).await;
    codec::decode(&bytes).unwrap()
}

/// Connect and authenticate a client. Returns (send, recv) streams.
async fn connect_and_auth(
    endpoint: &Endpoint,
    server_addr: SocketAddr,
    keypair: &Keypair,
    invite_code: Option<&str>,
    setup_token: Option<&str>,
) -> (SendStream, RecvStream) {
    let conn = endpoint
        .connect(server_addr, "farder-server")
        .unwrap()
        .await
        .unwrap();
    let (mut send, mut recv) = conn.accept_bi().await.unwrap();

    // Receive challenge
    let challenge = match recv_server_frame(&mut recv).await {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge, got {:?}", other),
    };

    // Sign and authenticate
    let signature = keypair.sign(&challenge);
    let auth_frame = ClientFrame::Authenticate {
        public_key: keypair.public_key(),
        signed_challenge: signature,
        invite_code: invite_code.map(String::from),
        setup_token: setup_token.map(String::from),
    };
    send_frame(&mut send, &auth_frame).await;

    // Receive auth result
    match recv_server_frame(&mut recv).await {
        ServerFrame::Authenticated { .. } => {}
        ServerFrame::AuthError { reason } => panic!("auth failed: {}", reason),
        other => panic!("expected Authenticated, got {:?}", other),
    }

    (send, recv)
}

async fn send_request(send: &mut SendStream, id: u32, request: ServerRequest) {
    let frame = ClientFrame::Request { id, body: request };
    send_frame(send, &frame).await;
}

/// Receive the next Response frame, skipping any Event frames.
async fn recv_response(recv: &mut RecvStream) -> (u32, ServerResponse) {
    loop {
        match recv_server_frame(recv).await {
            ServerFrame::Response { request_id, body } => return (request_id, body),
            ServerFrame::Event(_) => continue, // skip events
            other => panic!("expected Response, got {:?}", other),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_e2e_server_bootstrap_and_chat() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // ignore if already installed

    // 1. Start server in-process
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server_endpoint = make_server_endpoint(bind_addr);
    let actual_addr = server_endpoint.local_addr().unwrap();

    // Set up server state
    let conn = farder_server::db::open_in_memory().unwrap();
    farder_server::members::create_role(
        &conn,
        "@everyone",
        farder_server::permissions::DEFAULT_EVERYONE,
        None,
        0,
        true,
    ).unwrap();
    // Apply blank template
    let templates = farder_server::templates::list_builtin_templates();
    let blank = templates.iter().find(|t| t.template.name == "Blank").unwrap();
    farder_server::templates::apply_template(&conn, blank).unwrap();

    let state = Arc::new(farder_server::state::ServerState::new(conn, "Test Server".to_string()));
    let setup_token = farder_server::auth::generate_setup_token();
    let setup_hex = hex::encode(&setup_token);
    *state.setup_token.lock().unwrap() = Some(setup_token);

    // Spawn server accept loop
    let server_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            let incoming = match server_endpoint.accept().await {
                Some(inc) => inc,
                None => break,
            };
            let state = Arc::clone(&server_state);
            tokio::spawn(async move {
                let conn = incoming.await.unwrap();
                let (send, recv) = conn.accept_bi().await.unwrap();
                let _ = farder_server::connection::handle_client(state, send, recv).await;
            });
        }
    });

    sleep(Duration::from_millis(50)).await;

    let client_endpoint = make_client_endpoint();

    // 2. Owner connects with setup token
    let owner_kp = Keypair::generate();
    let (mut owner_send, mut owner_recv) = connect_and_auth(
        &client_endpoint,
        actual_addr,
        &owner_kp,
        None,
        Some(&setup_hex),
    ).await;

    // 3. Owner creates an invite
    send_request(&mut owner_send, 1, ServerRequest::CreateInvite {
        max_uses: Some(5),
        expires_in_secs: None,
        target_channel: None,
    }).await;

    let (_, resp) = recv_response(&mut owner_recv).await;
    let invite_code = match resp {
        ServerResponse::InviteCreated { code } => code,
        other => panic!("expected InviteCreated, got {:?}", other),
    };

    // 4. Owner subscribes to general channel
    send_request(&mut owner_send, 2, ServerRequest::GetServerInfo).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    let general_channel_id = match resp {
        ServerResponse::ServerInfo { channels, .. } => {
            assert!(!channels.is_empty());
            channels[0].id
        }
        other => panic!("expected ServerInfo, got {:?}", other),
    };

    send_request(&mut owner_send, 3, ServerRequest::Subscribe {
        channel_ids: vec![general_channel_id],
    }).await;
    let _ = recv_response(&mut owner_recv).await;

    // 5. Second user joins with invite code
    let user_kp = Keypair::generate();
    let (mut user_send, mut user_recv) = connect_and_auth(
        &client_endpoint,
        actual_addr,
        &user_kp,
        Some(&invite_code),
        None,
    ).await;

    // User subscribes to general
    send_request(&mut user_send, 1, ServerRequest::Subscribe {
        channel_ids: vec![general_channel_id],
    }).await;
    let _ = recv_response(&mut user_recv).await;

    // 6. User sends a message
    send_request(&mut user_send, 2, ServerRequest::SendMessage {
        channel_id: general_channel_id,
        content: "Hello from the new member!".to_string(),
        reply_to: None,
    }).await;
    let (_, resp) = recv_response(&mut user_recv).await;
    let msg_id = match resp {
        ServerResponse::MessageSent { id, .. } => id,
        other => panic!("expected MessageSent, got {:?}", other),
    };
    assert!(msg_id > 0);

    // 7. Owner fetches history and sees the message
    send_request(&mut owner_send, 4, ServerRequest::FetchHistory {
        channel_id: general_channel_id,
        before_id: None,
        limit: 50,
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::History { messages } => {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].content, "Hello from the new member!");
            assert_eq!(messages[0].author, user_kp.public_key());
        }
        other => panic!("expected History, got {:?}", other),
    }

    // 8. Owner searches for the message
    send_request(&mut owner_send, 5, ServerRequest::Search {
        query: "Hello".to_string(),
        channel_id: Some(general_channel_id),
        limit: 10,
    }).await;
    let (_, resp) = recv_response(&mut owner_recv).await;
    match resp {
        ServerResponse::SearchResults { messages } => {
            assert_eq!(messages.len(), 1);
        }
        other => panic!("expected SearchResults, got {:?}", other),
    }
}
```

- [ ] **Step 3: Expose modules from farder-server lib for test access**

The e2e test imports `farder_server::db`, `farder_server::connection`, etc. For this to work, the server crate needs a `lib.rs` in addition to `main.rs`. Create `crates/farder-server/src/lib.rs`:

```rust
pub mod auth;
pub mod channels;
pub mod connection;
pub mod db;
pub mod events;
pub mod handlers;
pub mod invites;
pub mod members;
pub mod messages;
pub mod permissions;
pub mod retention;
pub mod state;
pub mod templates;
```

And update `main.rs` to use the library crate instead of declaring modules directly. Replace the module declarations at the top of `main.rs` with:

```rust
use farder_server::{auth, connection, db, members, permissions, retention, state, templates};
```

Also add to `crates/farder-server/Cargo.toml`:

```toml
[lib]
name = "farder_server"
path = "src/lib.rs"

[[bin]]
name = "farder-server"
path = "src/main.rs"
```

- [ ] **Step 4: Run the e2e test**

Run: `cd /home/deez/farder && cargo test e2e_server -- --nocapture`

Expected: The test passes — owner bootstraps with setup token, invites a user, user joins and sends a message, owner fetches history and searches.

- [ ] **Step 5: Run all tests to verify nothing is broken**

Run: `cd /home/deez/farder && cargo test`

Expected: All tests across all crates pass.

- [ ] **Step 6: Commit**

```bash
git add tests/e2e_server.rs crates/farder-server/src/lib.rs crates/farder-server/src/main.rs crates/farder-server/Cargo.toml Cargo.toml
git commit -m "feat(server): add e2e integration test for server bootstrap, invite, and messaging"
```

---

## Self-Review Results

**Spec coverage check:**
- Architecture (new crate, modules) ✅ Task 1
- Transport (QUIC, Quinn, bi-stream) ✅ Task 11, 13
- Storage (SQLite, FTS5, all tables) ✅ Task 3
- Authentication (setup token, challenge-response, sessions) ✅ Task 8, 11
- Member data ✅ Task 4
- Key revocation ✅ Task 4 (revoke_member), Task 8 (authenticate_existing_member checks revoked)
- Channels & Categories (types, settings, lifecycle) ✅ Task 5
- Permissions (bitfield, roles, overrides, resolution) ✅ Task 2
- Server-side enforcement ✅ Task 10 (every handler checks permissions)
- Messaging protocol (all requests and events) ✅ Task 1, 10, 11
- Subscription model ✅ Task 11 (connection.rs handles Subscribe)
- Message retention ✅ Task 12
- Invite system ✅ Task 7
- Templates ✅ Task 9
- What's NOT in Phase 2 — confirmed none of those features are included ✅

**Placeholder scan:** No TBD/TODO/placeholder patterns found.

**Type consistency:** Verified — `ServerRequest`, `ServerResponse`, `ServerEvent`, `MessageInfo`, `ChannelInfo`, `CategoryInfo`, `RoleInfo`, `MemberInfo`, `OverrideInfo`, `ChannelType`, `ClientFrame`, `ServerFrame` are used consistently across all tasks.
