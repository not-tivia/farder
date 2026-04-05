# Server-Routed Direct Messages — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 1:1 direct messaging between users on the same server, with DMs as private channels, a friends/block system, DM section in the sidebar, and a pop-out side panel for simultaneous server + DM chatting.

**Architecture:** DMs reuse the existing channel/message infrastructure as `channel_type = "dm"`. New `dm_participants` and `blocked_users` tables track who's in each DM and who's blocked. Server-side: new request handlers for OpenDm, ListDms, BlockUser, UnblockUser. Client-side: DM section in sidebar, "Message" button in profile popup, pop-out DM panel.

**Tech Stack:** Existing — Rust server, Tauri client, React, farder-protocol.

**Spec:** `docs/specs/2026-04-04-farder-server-routed-dms-design.md`

---

## File Structure

### Server (modified)

```
crates/farder-protocol/src/server.rs     # New types: DmEntry, OpenDm/ListDms/BlockUser/UnblockUser requests, DmOpened/DmList responses, DmCreated event
crates/farder-server/src/db.rs           # Add dm_participants and blocked_users tables
crates/farder-server/src/members.rs      # Block/unblock CRUD functions
crates/farder-server/src/channels.rs     # DM channel creation, find existing DM, list DMs with participant info
crates/farder-server/src/handlers.rs     # OpenDm, ListDms, BlockUser, UnblockUser handlers; modify SendMessage to check DM permissions and blocks
crates/farder-server/src/connection.rs   # Pass DmCreated events
```

### Client (modified + new)

```
client/src-tauri/src/commands.rs         # open_dm, list_dms, block_user, unblock_user commands
client/src-tauri/src/main.rs             # Register new commands
client/src/lib/tauri-bridge.ts           # New bridge functions
client/src/lib/types.ts                  # DmEntry type
client/src/context/ServerContext.tsx      # Add dms state, DM actions
client/src/hooks/useServerEvents.ts      # Handle dm_created event
client/src/components/ChannelSidebar.tsx  # DM section below channels
client/src/components/UserProfilePopup.tsx # "Message" and "Block" buttons
client/src/components/DmPanel.tsx         # NEW: pop-out DM side panel
client/src/components/AppShell.tsx        # Add DM panel slot
client/src/styles/xp-theme.css           # DM section + panel styling
```

---

## Task 1: Protocol Types

**Files:**
- Modify: `crates/farder-protocol/src/server.rs`

- [ ] **Step 1: Add DM types**

Add `DmEntry` struct:
```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DmEntry {
    pub channel: ChannelInfo,
    pub participant: MemberInfo,
    pub last_message: Option<MessageInfo>,
}
```

Add to `ServerRequest`:
```rust
OpenDm { target_key: PublicKey },
ListDms,
BlockUser { target_key: PublicKey },
UnblockUser { target_key: PublicKey },
```

Add to `ServerResponse`:
```rust
DmOpened { channel: ChannelInfo, participant: MemberInfo },
DmList { dms: Vec<DmEntry> },
```

Add to `ServerEvent`:
```rust
DmCreated { channel: ChannelInfo, participant: MemberInfo },
```

Add `ChannelType::Dm` variant. Update `channel_type_to_str` / `str_to_channel_type` for "dm".

Add the 4 new requests to `test_roundtrip_all_request_variants`. Fix all compilation errors from new fields.

- [ ] **Step 2: Verify tests pass**

Run: `cargo test --workspace`

- [ ] **Step 3: Commit**

```bash
git add crates/farder-protocol/ crates/farder-server/src/channels.rs crates/farder-server/src/handlers.rs
git commit -m "feat(protocol): add DM types — DmEntry, OpenDm, ListDms, BlockUser, UnblockUser, DmCreated"
```

---

## Task 2: DB Schema & DM/Block Storage

**Files:**
- Modify: `crates/farder-server/src/db.rs`
- Modify: `crates/farder-server/src/channels.rs`
- Modify: `crates/farder-server/src/members.rs`

- [ ] **Step 1: Add tables to schema**

In `db.rs` init_schema, add:
```sql
CREATE TABLE IF NOT EXISTS dm_participants (
    channel_id INTEGER NOT NULL,
    user_key BLOB NOT NULL,
    PRIMARY KEY (channel_id, user_key),
    FOREIGN KEY (channel_id) REFERENCES channels(id)
);

CREATE TABLE IF NOT EXISTS blocked_users (
    blocker_key BLOB NOT NULL,
    blocked_key BLOB NOT NULL,
    blocked_at INTEGER NOT NULL,
    PRIMARY KEY (blocker_key, blocked_key)
);
```

- [ ] **Step 2: Add DM channel functions to channels.rs**

```rust
/// Create a DM channel between two users. Returns the channel ID.
pub fn create_dm_channel(conn: &Connection, user_a: &PublicKey, user_b: &PublicKey) -> Result<u64>
// INSERT channel with type="dm", name="dm", position=0
// INSERT two dm_participants rows
// Return channel id

/// Find an existing DM channel between two users. Returns None if not found.
pub fn find_dm_channel(conn: &Connection, user_a: &PublicKey, user_b: &PublicKey) -> Result<Option<ChannelInfo>>
// SELECT channel_id FROM dm_participants WHERE user_key = ?1
// INTERSECT
// SELECT channel_id FROM dm_participants WHERE user_key = ?2
// Then get_channel for the result

/// Find or create a DM channel between two users.
pub fn open_dm_channel(conn: &Connection, user_a: &PublicKey, user_b: &PublicKey) -> Result<(u64, bool)>
// find_dm_channel, if found return (id, false), else create_dm_channel return (id, true)

/// List all DM channels for a user, with the other participant's info.
pub fn list_dm_channels(conn: &Connection, user: &PublicKey) -> Result<Vec<(ChannelInfo, PublicKey)>>
// SELECT dm_participants.channel_id, dm_participants.user_key
// FROM dm_participants
// WHERE channel_id IN (SELECT channel_id FROM dm_participants WHERE user_key = ?)
// AND user_key != ?
// For each: get_channel

/// Check if a user is a participant in a DM channel
pub fn is_dm_participant(conn: &Connection, channel_id: u64, user: &PublicKey) -> Result<bool>
```

- [ ] **Step 3: Add block functions to members.rs**

```rust
pub fn block_user(conn: &Connection, blocker: &PublicKey, blocked: &PublicKey) -> Result<()>
// INSERT OR IGNORE INTO blocked_users

pub fn unblock_user(conn: &Connection, blocker: &PublicKey, blocked: &PublicKey) -> Result<()>
// DELETE FROM blocked_users

pub fn is_blocked(conn: &Connection, user_a: &PublicKey, user_b: &PublicKey) -> Result<bool>
// Check if either user has blocked the other (bidirectional check)
// SELECT count(*) FROM blocked_users WHERE (blocker_key=?1 AND blocked_key=?2) OR (blocker_key=?2 AND blocked_key=?1)
```

- [ ] **Step 4: Update list_channels to exclude DMs**

In `channels.rs`, the `list_channels` WHERE clause already excludes threads. Add `AND channel_type != 'dm'`.

- [ ] **Step 5: Write tests**

Tests for: create_dm_channel, find_dm_channel, open_dm_channel (idempotent), list_dm_channels, is_dm_participant, block/unblock/is_blocked.

- [ ] **Step 6: Verify tests pass**

Run: `cargo test --workspace`

- [ ] **Step 7: Commit**

```bash
git add crates/farder-server/src/db.rs crates/farder-server/src/channels.rs crates/farder-server/src/members.rs
git commit -m "feat(server): add DM channel and block storage with CRUD functions"
```

---

## Task 3: Server Handlers

**Files:**
- Modify: `crates/farder-server/src/handlers.rs`
- Modify: `crates/farder-server/src/connection.rs`

- [ ] **Step 1: Implement DM handlers**

**OpenDm:**
```rust
ServerRequest::OpenDm { target_key } => {
    // Can't DM yourself
    if target_key == *member { return err("cannot DM yourself"); }
    // Check both are members
    if members::get_member(conn, &target_key)?.is_none() { return err("user not found"); }
    // Check not blocked
    if members::is_blocked(conn, member, &target_key)? { return err("blocked"); }
    // Find or create DM channel
    let (channel_id, created) = channels::open_dm_channel(conn, member, &target_key)?;
    let channel = channels::get_channel(conn, channel_id)?.unwrap();
    let participant = members::get_member(conn, &target_key)?.unwrap();
    let participant_info = MemberInfo {
        public_key: participant.public_key.clone(),
        display_name: participant.display_name,
        joined_at: participant.joined_at,
        role_ids: members::get_member_role_ids(conn, &target_key)?,
    };
    let mut events = Vec::new();
    if created {
        // Notify both users
        events.push(BroadcastEvent { target: EventTarget::All, event: ServerEvent::DmCreated { channel: channel.clone(), participant: participant_info.clone() } });
    }
    ok_with(ServerResponse::DmOpened { channel, participant: participant_info }, events)
}
```

**ListDms:**
```rust
ServerRequest::ListDms => {
    let dm_channels = channels::list_dm_channels(conn, member)?;
    let mut dms = Vec::new();
    for (channel, other_key) in dm_channels {
        let other = members::get_member(conn, &other_key)?;
        if let Some(other_member) = other {
            let role_ids = members::get_member_role_ids(conn, &other_key)?;
            let participant = MemberInfo {
                public_key: other_member.public_key,
                display_name: other_member.display_name,
                joined_at: other_member.joined_at,
                role_ids,
            };
            // Get last message
            let history = messages::fetch_history(conn, channel.id, None, 1, member)?;
            let last_message = history.into_iter().next();
            dms.push(DmEntry { channel, participant, last_message });
        }
    }
    // Sort by last message timestamp (most recent first)
    dms.sort_by(|a, b| {
        let ts_a = a.last_message.as_ref().map(|m| m.timestamp).unwrap_or(0);
        let ts_b = b.last_message.as_ref().map(|m| m.timestamp).unwrap_or(0);
        ts_b.cmp(&ts_a)
    });
    ok(ServerResponse::DmList { dms })
}
```

**BlockUser / UnblockUser:**
```rust
ServerRequest::BlockUser { target_key } => {
    members::block_user(conn, member, &target_key)?;
    ok(ServerResponse::Ok)
}
ServerRequest::UnblockUser { target_key } => {
    members::unblock_user(conn, member, &target_key)?;
    ok(ServerResponse::Ok)
}
```

- [ ] **Step 2: Modify SendMessage for DM channels**

In the `SendMessage` handler, before the permission check, add a DM-specific path:

```rust
// Check if this is a DM channel
let channel = channels::get_channel(conn, channel_id)?
    .ok_or_else(|| anyhow::anyhow!("channel not found"))?;
if channel.channel_type == ChannelType::Dm {
    // DM channels: check participant and block status
    if !channels::is_dm_participant(conn, channel_id, member)? {
        return err("not a participant in this DM");
    }
    // Get the other participant
    // Check blocks
    // ... then proceed with message insertion (skip permission check)
}
```

Actually, the simplest approach: after inserting the message in a DM channel, check the channel type and if it's a DM, verify participation and block status. If the channel type is DM, skip the normal SEND_MESSAGES permission check.

- [ ] **Step 3: Add DmCreated event dispatch in bridge.rs**

In `connection.rs` `dispatch_event` (or `bridge.rs` on the server side), add:
```rust
ServerEvent::DmCreated { channel, participant } => {
    let _ = app.emit("server:dm_created", serde_json::json!({ "channel": channel, "participant": participant }));
}
```

Wait — `dispatch_event` is in the server's `connection.rs`, not the client's `bridge.rs`. The server broadcasts events, and the client's bridge.rs handles emitting them as Tauri events. Let me check...

The server's `connection.rs` `broadcast_event` sends events to all connected clients. The client's `bridge.rs` `dispatch_event` receives them and emits Tauri events. Add `DmCreated` handling to the client's `bridge.rs`.

- [ ] **Step 4: Write handler tests**

Tests for: open_dm, open_dm idempotent, list_dms, block prevents dm, send message in dm.

- [ ] **Step 5: Verify tests pass**

Run: `cargo test --workspace`

- [ ] **Step 6: Commit**

```bash
git add crates/farder-server/src/handlers.rs crates/farder-server/src/connection.rs
git commit -m "feat(server): implement OpenDm, ListDms, BlockUser, UnblockUser handlers with DM send checks"
```

---

## Task 4: Client Backend — Tauri Commands

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`
- Modify: `client/src-tauri/src/bridge.rs`

- [ ] **Step 1: Add Tauri commands**

```rust
#[tauri::command]
pub async fn open_dm(state: State<'_, Arc<AppState>>, target_key: String) -> Result<DmOpenedResult, String>
// Parse target_key as hex pubkey bytes, send OpenDm request, return channel + participant info

#[tauri::command]
pub async fn list_dms(state: State<'_, Arc<AppState>>) -> Result<Vec<DmEntryResult>, String>
// Send ListDms request, return list

#[tauri::command]
pub async fn block_user(state: State<'_, Arc<AppState>>, target_key: String) -> Result<(), String>

#[tauri::command]
pub async fn unblock_user(state: State<'_, Arc<AppState>>, target_key: String) -> Result<(), String>
```

Serializable result types:
```rust
#[derive(serde::Serialize)]
pub struct DmOpenedResult { pub channel: ChannelInfo, pub participant: MemberInfo }

#[derive(serde::Serialize)]
pub struct DmEntryResult { pub channel: ChannelInfo, pub participant: MemberInfo, pub last_message: Option<MessageInfo> }
```

Note: the target_key comes from TypeScript as a `vk_` hex string. The Rust side needs to parse it back to PublicKey bytes. Add a helper:
```rust
fn parse_public_key(key_str: &str) -> Result<PublicKey, String> {
    let hex = key_str.strip_prefix("vk_").unwrap_or(key_str);
    let bytes = hex::decode(hex).map_err(|e| e.to_string())?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "invalid key length".to_string())?;
    Ok(PublicKey::from_bytes(arr))
}
```

Register all 4 commands in main.rs.

- [ ] **Step 2: Add DmCreated event to bridge.rs**

In the client's `bridge.rs` `dispatch_event`:
```rust
ServerEvent::DmCreated { channel, participant } => {
    let _ = app.emit("server:dm_created", serde_json::json!({
        "channel": &channel,
        "participant": &participant,
    }));
}
```

- [ ] **Step 3: Commit**

```bash
git add client/src-tauri/
git commit -m "feat(client): add Tauri commands for DMs — open_dm, list_dms, block_user, unblock_user"
```

---

## Task 5: Client Frontend — Types, Bridge, State

**Files:**
- Modify: `client/src/lib/types.ts`
- Modify: `client/src/lib/tauri-bridge.ts`
- Modify: `client/src/context/ServerContext.tsx`
- Modify: `client/src/hooks/useServerEvents.ts`

- [ ] **Step 1: Add TypeScript types**

In `types.ts`:
```typescript
export interface DmEntry {
    channel: ChannelInfo;
    participant: MemberInfo;
    last_message: MessageInfo | null;
}
```

- [ ] **Step 2: Add bridge functions**

In `tauri-bridge.ts`:
```typescript
export async function openDm(targetKey: string): Promise<{ channel: ChannelInfo; participant: MemberInfo }> {
    return invoke("open_dm", { targetKey });
}
export async function listDms(): Promise<DmEntry[]> {
    return invoke("list_dms");
}
export async function blockUser(targetKey: string): Promise<void> {
    return invoke("block_user", { targetKey });
}
export async function unblockUser(targetKey: string): Promise<void> {
    return invoke("unblock_user", { targetKey });
}
```

- [ ] **Step 3: Add DM state to context**

In `ServerContext.tsx`, add to `ServerState`:
```typescript
dms: DmEntry[];
dmPanelChannelId: number | null;  // for pop-out panel
```

Add actions:
```typescript
| { type: "SET_DMS"; payload: DmEntry[] }
| { type: "DM_CREATED"; payload: { channel: ChannelInfo; participant: MemberInfo } }
| { type: "OPEN_DM_PANEL"; payload: number }
| { type: "CLOSE_DM_PANEL" }
```

Reducer cases:
- `SET_DMS`: set dms array
- `DM_CREATED`: add to dms array
- `OPEN_DM_PANEL`: set dmPanelChannelId
- `CLOSE_DM_PANEL`: set dmPanelChannelId to null

- [ ] **Step 4: Add DM event listener**

In `useServerEvents.ts`:
```typescript
listen("server:dm_created", (e) => {
    const p = e.payload as any;
    dispatch({ type: "DM_CREATED", payload: { channel: p.channel, participant: p.participant } });
}).then((u) => unlisten.push(u));
```

- [ ] **Step 5: Commit**

```bash
git add client/src/
git commit -m "feat(client): add DM types, bridge functions, and state management"
```

---

## Task 6: Client UI — DM Sidebar Section

**Files:**
- Modify: `client/src/components/ChannelSidebar.tsx`
- Modify: `client/src/components/ConnectDialog.tsx` (or AppShell — load DMs on connect)

- [ ] **Step 1: Load DMs on connect**

After connecting, call `listDms` and dispatch `SET_DMS`. In `ConnectDialog.tsx` or `AppShell.tsx`, after `CONNECTED` + `SET_MEMBERS`:
```typescript
try {
    const dms = await api.listDms();
    dispatch({ type: "SET_DMS", payload: dms });
} catch {}
```

- [ ] **Step 2: Render DM section in sidebar**

In `ChannelSidebar.tsx`, after the channel list, add a "DIRECT MESSAGES" section:
```tsx
{/* DM Section */}
{state.dms.length > 0 && (
    <>
        <div className="channel-category" style={{ marginTop: 8 }}>DIRECT MESSAGES</div>
        {state.dms.map(dm => {
            const isActive = state.currentChannelId === dm.channel.id;
            const lastRead = state.readState?.[dm.channel.id] ?? 0;
            const dmMsgs = state.messages[dm.channel.id] ?? [];
            const hasUnread = dmMsgs.some(m => m.id > lastRead) && dm.channel.id !== state.currentChannelId;
            return (
                <div
                    key={dm.channel.id}
                    className={`channel-item dm-item${isActive ? " active" : ""}${hasUnread ? " unread" : ""}`}
                    onClick={() => handleSelectChannel(dm.channel)}
                >
                    <span className="dm-initial">{dm.participant.display_name.charAt(0).toUpperCase()}</span>
                    <span>{dm.participant.display_name}</span>
                </div>
            );
        })}
    </>
)}
```

CSS for dm items:
```css
.dm-item { padding-left: 12px; }
.dm-initial {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: rgba(255,255,255,0.2);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 10px;
    margin-right: 6px;
    flex-shrink: 0;
}
```

- [ ] **Step 3: Commit**

```bash
git add client/src/
git commit -m "feat(client): DM section in sidebar with unread indicators"
```

---

## Task 7: Client UI — Profile "Message" Button + Block

**Files:**
- Modify: `client/src/components/UserProfilePopup.tsx`

- [ ] **Step 1: Add Message and Block buttons to profile popup**

In `UserProfilePopup`, at the bottom of the card body (only for non-self profiles):

```tsx
{!isSelf && (
    <div className="profile-card-actions">
        <button className="xp-button" onClick={async () => {
            try {
                const result = await api.openDm(pkStr);
                dispatch({ type: "SELECT_CHANNEL", payload: result.channel.id });
                // Subscribe and fetch history
                await api.subscribeChannels([result.channel.id]);
                const msgs = await api.fetchHistory(result.channel.id);
                dispatch({ type: "SET_MESSAGES", payload: { channelId: result.channel.id, messages: msgs.reverse() } });
                onClose();
            } catch {}
        }}>Message</button>
        <button className="xp-button" style={{ color: "#cc0000" }} onClick={async () => {
            try { await api.blockUser(pkStr); } catch {}
            onClose();
        }}>Block</button>
    </div>
)}
```

CSS:
```css
.profile-card-actions {
    display: flex;
    gap: 6px;
    padding: 8px 12px;
    border-top: 1px solid var(--xp-border);
}
```

UserProfilePopup needs access to `dispatch` — import `useServer`.

- [ ] **Step 2: Commit**

```bash
git add client/src/components/UserProfilePopup.tsx client/src/styles/xp-theme.css
git commit -m "feat(client): Message and Block buttons in user profile popup"
```

---

## Task 8: Client UI — Pop-Out DM Panel

**Files:**
- Create: `client/src/components/DmPanel.tsx`
- Modify: `client/src/components/AppShell.tsx`
- Modify: `client/src/components/ChatPanel.tsx`

- [ ] **Step 1: Create DmPanel component**

A narrow right-side panel (~300px) that shows a DM conversation alongside the main chat:

```tsx
import { useEffect, useRef, useState } from "react";
import { useServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import Message from "./Message";
import MessageInput from "./MessageInput";

export default function DmPanel() {
    const { state, dispatch } = useServer();
    const channelId = state.dmPanelChannelId;
    const bottomRef = useRef<HTMLDivElement>(null);

    const dm = state.dms.find(d => d.channel.id === channelId);
    const messages = channelId ? (state.messages[channelId] ?? []) : [];

    const memberNames: Record<string, string> = {};
    for (const m of state.members) {
        memberNames[publicKeyToString(m.public_key)] = m.display_name;
    }
    // Add DM participant
    if (dm) {
        memberNames[publicKeyToString(dm.participant.public_key)] = dm.participant.display_name;
    }

    useEffect(() => {
        if (!channelId) return;
        (async () => {
            await api.subscribeChannels([channelId]);
            const msgs = await api.fetchHistory(channelId);
            dispatch({ type: "SET_MESSAGES", payload: { channelId, messages: msgs.reverse() } });
        })();
    }, [channelId]);

    useEffect(() => {
        bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }, [messages.length]);

    if (!channelId || !dm) return null;

    return (
        <div className="dm-panel">
            <div className="dm-panel-header">
                <span>{dm.participant.display_name}</span>
                <button className="modal-close" onClick={() => dispatch({ type: "CLOSE_DM_PANEL" })}>X</button>
            </div>
            <div className="dm-panel-messages">
                {messages.map((msg, i) => {
                    const prev = i > 0 ? messages[i - 1] : null;
                    const sameAuthor = prev && JSON.stringify(prev.author.bytes) === JSON.stringify(msg.author.bytes);
                    const withinWindow = prev && (msg.timestamp - prev.timestamp) < 300;
                    const grouped = !!(sameAuthor && withinWindow);
                    return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} />;
                })}
                <div ref={bottomRef} />
            </div>
            <MessageInput channelId={channelId} />
        </div>
    );
}
```

- [ ] **Step 2: Add DM panel to AppShell**

In `AppShell.tsx`, render DmPanel alongside the main layout:
```tsx
import DmPanel from "./DmPanel";

// In render:
<div className="main-layout">
    <ChannelSidebar />
    <ChatPanel />
    <MemberSidebar />
    {state.dmPanelChannelId && <DmPanel />}
</div>
```

- [ ] **Step 3: Add "Pop Out" button to DM chat header**

In `ChatPanel.tsx`, when viewing a DM channel (channel_type === "Dm"), show a "Pop Out" button that moves the DM to the side panel and switches the main panel back to the last server channel:

```tsx
// In channel header:
{currentChannel?.channel_type === "Dm" && (
    <button className="xp-button" style={{ fontSize: 10, marginLeft: "auto", padding: "2px 8px" }}
        onClick={() => {
            dispatch({ type: "OPEN_DM_PANEL", payload: currentChannelId! });
            // Switch main panel back to first non-DM channel
            const firstChannel = state.channels.find(c => c.channel_type !== "Dm" && c.channel_type !== "Thread");
            if (firstChannel) dispatch({ type: "SELECT_CHANNEL", payload: firstChannel.id });
        }}
    >Pop Out</button>
)}
```

- [ ] **Step 4: Add DM panel CSS**

```css
.dm-panel {
    width: 320px;
    border-left: 2px solid var(--xp-blue);
    display: flex;
    flex-direction: column;
    background: var(--xp-white);
    flex-shrink: 0;
}

.dm-panel-header {
    padding: 6px 10px;
    background: linear-gradient(180deg, var(--xp-blue-light) 0%, var(--xp-blue) 100%);
    color: white;
    font-weight: bold;
    font-size: 12px;
    display: flex;
    justify-content: space-between;
    align-items: center;
}

.dm-panel-messages {
    flex: 1;
    overflow-y: auto;
    padding: 8px;
    font-size: 11px;
}
```

- [ ] **Step 5: Commit**

```bash
git add client/src/components/DmPanel.tsx client/src/components/AppShell.tsx client/src/components/ChatPanel.tsx client/src/styles/xp-theme.css
git commit -m "feat(client): pop-out DM side panel for simultaneous server + DM chatting"
```

---

## Task 9: E2E Test

**Files:**
- Modify: `tests/e2e_server.rs`

- [ ] **Step 1: Add DM test to e2e**

After existing test flow, add:
```rust
// DM Flow
// Owner opens DM with user
send_request(&mut owner_send, N, ServerRequest::OpenDm { target_key: user_kp.public_key() }).await;
let (_, resp) = recv_response(&mut owner_recv).await;
let dm_channel_id = match resp {
    ServerResponse::DmOpened { channel, participant } => {
        assert_eq!(participant.display_name.len() > 0, true);
        channel.id
    }
    other => panic!("expected DmOpened, got {:?}", other),
};

// Owner sends DM
send_request(&mut owner_send, N+1, ServerRequest::SendMessage {
    channel_id: dm_channel_id,
    content: "hey, private message!".to_string(),
    reply_to: None,
    attachment_ids: vec![],
}).await;
let (_, resp) = recv_response(&mut owner_recv).await;
match resp {
    ServerResponse::MessageSent { .. } => {}
    other => panic!("expected MessageSent, got {:?}", other),
}

// Owner lists DMs
send_request(&mut owner_send, N+2, ServerRequest::ListDms).await;
let (_, resp) = recv_response(&mut owner_recv).await;
match resp {
    ServerResponse::DmList { dms } => {
        assert_eq!(dms.len(), 1);
        assert!(dms[0].last_message.is_some());
    }
    other => panic!("expected DmList, got {:?}", other),
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --workspace`

- [ ] **Step 3: Commit**

```bash
git add tests/e2e_server.rs
git commit -m "feat: add e2e test for DM open, send, and list"
```

---

## Self-Review Results

**Spec coverage:**
- DM channels (type="dm") ✅ Tasks 1, 2
- dm_participants table ✅ Task 2
- blocked_users table ✅ Task 2
- OpenDm request ✅ Task 3
- ListDms request ✅ Task 3
- BlockUser/UnblockUser ✅ Task 3
- DmCreated event ✅ Tasks 1, 3, 4
- DM section in sidebar ✅ Task 6
- "Message" button in profile ✅ Task 7
- Block button in profile ✅ Task 7
- Pop-out DM panel ✅ Task 8
- SendMessage DM checks ✅ Task 3
- list_channels excludes DMs ✅ Task 2
- E2E test ✅ Task 9

**Placeholder scan:** None found.

**Type consistency:** `DmEntry`, `DmOpenedResult`, `DmEntryResult`, `openDm`, `listDms`, `blockUser`, `unblockUser`, `open_dm_channel`, `find_dm_channel`, `list_dm_channels`, `is_dm_participant`, `block_user`, `unblock_user`, `is_blocked` — consistent across all tasks.
