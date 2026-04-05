# Farder: Multi-Server Support — Design Spec

**Date:** 2026-04-05
**Status:** Draft
**Depends On:** Client v1, DM support

## Goal

Support simultaneous connections to multiple Farder servers. Users see a Discord-style server icon strip on the far left, click to switch between servers, and receive notifications (mentions, DMs, announcements) from background servers without full real-time message processing.

## Architecture

### Backend (Tauri)

The single `AppState` connection is replaced with a `MultiServerState` holding multiple connections.

```
MultiServerState {
    signing_key_bytes: Mutex<Option<[u8; 32]>>,
    servers: Mutex<HashMap<String, ServerConnection>>,  // keyed by server address
}

ServerConnection {
    endpoint: Endpoint,
    connection: Connection,
    send_stream: tokio::sync::Mutex<SendStream>,
    next_request_id: AtomicU32,
    pending_requests: Mutex<HashMap<u32, oneshot::Sender<ServerResponse>>>,
    event_reader_handle: JoinHandle<()>,
    server_name: String,
}
```

All IPC commands gain a `server_id: String` parameter (the server address) to target the correct connection. The `connect_server` command inserts a new `ServerConnection` into the map. `disconnect_server` removes one.

Each server's event reader runs independently, emitting Tauri events with a `server_id` field:

```rust
app.emit("server:new_message", json!({
    "server_id": server_address,
    "payload": message,
}));
```

### Frontend (React)

**State structure:**

```typescript
interface AppState {
    activeServerId: string | null;
    serverList: ServerListEntry[];        // lightweight, for the strip
    activeServerState: ServerState | null; // full state for the active server only
}

interface ServerListEntry {
    id: string;          // server address
    name: string;
    connected: boolean;
    unreadCount: number;
    hasMention: boolean;
}
```

Full channel/message/member state is only maintained for the `activeServerId`. When switching servers, the old state is discarded and fresh data is fetched for the new server.

**Event routing:**

All Tauri events now include `server_id`. The event hook checks:
- If `server_id === activeServerId`: process normally (update messages, channels, etc.)
- If `server_id !== activeServerId`: only process notification-relevant events:
  - DM messages → increment unreadCount, set hasMention
  - Mentions (@user) → set hasMention (requires server-side mention detection — deferred, just increment unreadCount for now)
  - Announcement channel messages → increment unreadCount

## UI Components

### Server Strip

A narrow (~48px) vertical strip on the far left of the window, containing:
- One circle per connected server showing the first letter of the server name
- Active server highlighted (brighter background, rounded square instead of circle)
- Unread dot indicator on servers with new messages
- Mention badge (red dot) on servers with mentions/DMs
- `+` button at the bottom to add a new server
- Separator line between the server icons and the `+` button

### Add Server Modal

Clicking `+` opens the same invite link input as the current join flow, but as a modal overlay. The user pastes a farder.gg invite link or raw address + code. Connection happens in the background — existing servers stay connected.

### Server Switching

Clicking a server icon in the strip:
1. Sets `activeServerId` to the clicked server
2. Fetches `GetServerInfo`, `GetMembers`, `ListDms` for that server
3. Populates `activeServerState` with the response
4. Resets unreadCount and hasMention for that server
5. Subscribes to the first channel

### First Launch Flow

If no servers are saved (first launch), the full-screen connect dialog appears as before. After connecting to the first server, the server strip appears and subsequent servers are added via the `+` button.

## Persistence

Server connections are saved to `~/.farder/servers.json`:

```json
[
    { "address": "127.0.0.1:4435", "name": "My Server" },
    { "address": "play.farder.gg:4435", "name": "Gaming Hub" }
]
```

On launch, the client auto-connects to all saved servers (sequentially, with the same identity). The first server in the list becomes the active one.

## Protocol Changes

None. The server protocol is unchanged. All multi-server logic is client-side.

## Command Changes

All existing commands gain a `server_id: String` parameter:
- `send_message(server_id, channel_id, content, ...)`
- `fetch_history(server_id, channel_id, ...)`
- `subscribe_channels(server_id, channel_ids)`
- `get_server_info(server_id)`
- `get_members(server_id)`
- `list_dms(server_id)`
- `open_dm(server_id, target_key)`
- etc.

New commands:
- `list_servers() -> Vec<ServerListEntry>` — returns all connected servers
- `switch_server(server_id)` — no-op on backend, just returns server info
- `save_servers(servers: Vec<{address, name}>)` — persists to disk

## What's NOT Included

- Per-server notification preferences (mute, etc.)
- Server-side mention detection (@user parsing)
- Server reordering in the strip
- Server icons/avatars (uses first letter for now)
- Background reconnection per-server (only active server auto-reconnects for now)
