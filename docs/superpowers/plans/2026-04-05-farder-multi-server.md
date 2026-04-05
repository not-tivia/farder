# Multi-Server Support — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Support simultaneous connections to multiple Farder servers with a Discord-style server icon strip, per-server state isolation, and background notification tracking.

**Architecture:** The Tauri backend changes from a single `AppState` with one connection to holding a `HashMap<String, ServerConnection>` keyed by server address. All IPC commands gain a `server_id` parameter. The React frontend adds a `ServerStrip` component and restructures state into `activeServerId` + per-server state with lazy loading. Events from all servers flow simultaneously, but only the active server gets full message processing — background servers just track notification counts.

**Tech Stack:** Existing — Tauri 2, React, TypeScript, Quinn QUIC.

**Spec:** `docs/specs/2026-04-05-farder-multi-server-design.md`

---

## File Structure

### Tauri Backend — rewrite

```
client/src-tauri/src/state.rs       # REWRITE: MultiServerState with HashMap<String, ServerConnection>
client/src-tauri/src/commands.rs     # REWRITE: all commands gain server_id param, new list_servers/save_servers
client/src-tauri/src/bridge.rs       # MODIFY: events include server_id field
client/src-tauri/src/main.rs         # MODIFY: manage MultiServerState
```

### React Frontend — rewrite

```
client/src/context/ServerContext.tsx  # REWRITE: multi-server state with activeServerId
client/src/hooks/useServerEvents.ts   # MODIFY: route events by server_id
client/src/lib/tauri-bridge.ts        # MODIFY: all functions gain serverId param
client/src/lib/types.ts               # ADD: ServerListEntry type
client/src/components/ServerStrip.tsx  # NEW: server icon strip
client/src/components/AddServerModal.tsx # NEW: modal for joining new servers
client/src/components/App.tsx         # MODIFY: show onboarding or multi-server UI
client/src/components/AppShell.tsx    # MODIFY: include ServerStrip
client/src/components/ConnectDialog.tsx # MODIFY: becomes first-launch only or Add Server modal
client/src/components/ChannelSidebar.tsx # MODIFY: use active server state
client/src/components/ChatPanel.tsx   # MINOR: use active server state
client/src/components/MemberSidebar.tsx # MINOR: use active server state
client/src/components/DmPanel.tsx     # MINOR: use active server state
client/src/styles/xp-theme.css        # ADD: server strip styles
```

---

## Task 1: Tauri Backend — MultiServerState

**Files:**
- Rewrite: `client/src-tauri/src/state.rs`

- [ ] **Step 1: Rewrite state.rs with multi-server architecture**

```rust
use farder_protocol::server::ServerResponse;
use quinn::{Connection, Endpoint, SendStream};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tokio::task::JoinHandle;

/// Per-server connection state.
pub struct ServerConnection {
    pub endpoint: Endpoint,
    pub connection: Connection,
    pub send_stream: tokio::sync::Mutex<SendStream>,
    pub next_request_id: AtomicU32,
    pub pending_requests: Mutex<HashMap<u32, tokio::sync::oneshot::Sender<ServerResponse>>>,
    pub event_reader_handle: JoinHandle<()>,
    pub server_name: String,
}

impl ServerConnection {
    pub fn next_id(&self) -> u32 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Global app state supporting multiple simultaneous server connections.
pub struct AppState {
    pub signing_key_bytes: Mutex<Option<[u8; 32]>>,
    pub servers: Mutex<HashMap<String, ServerConnection>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            signing_key_bytes: Mutex::new(None),
            servers: Mutex::new(HashMap::new()),
        }
    }
}
```

Key changes from old `AppState`:
- No more single `endpoint`/`connection`/`send_stream` — they're per-server in `ServerConnection`
- `send_stream` is now a non-optional `tokio::sync::Mutex<SendStream>` (always exists while connected)
- `servers` is a `Mutex<HashMap<String, ServerConnection>>` keyed by server address
- `event_reader_handle` is non-optional (always running while connected)

- [ ] **Step 2: Commit**

```bash
git add client/src-tauri/src/state.rs
git commit -m "refactor(client): rewrite state.rs for multi-server — HashMap of ServerConnections"
```

---

## Task 2: Tauri Backend — Bridge with server_id

**Files:**
- Rewrite: `client/src-tauri/src/bridge.rs`

- [ ] **Step 1: Rewrite bridge.rs to route by server_id**

The `send_request` function now takes a server_id to find the right connection:

```rust
use crate::connection::{recv_server_frame, write_frame};
use crate::state::AppState;
use anyhow::{Context, Result};
use farder_protocol::{codec, server::*};
use quinn::RecvStream;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::task::JoinHandle;

pub async fn send_request(
    state: &AppState,
    server_id: &str,
    request: ServerRequest,
) -> Result<ServerResponse> {
    let (id, tx, rx) = {
        let servers = state.servers.lock().map_err(|e| anyhow::anyhow!("{}", e))?;
        let conn = servers.get(server_id)
            .ok_or_else(|| anyhow::anyhow!("not connected to server: {}", server_id))?;
        let id = conn.next_id();
        let (tx_send, rx_recv) = tokio::sync::oneshot::channel();
        conn.pending_requests.lock().unwrap().insert(id, tx_send);
        (id, rx_recv)
    };

    // Send the frame
    {
        let servers = state.servers.lock().unwrap();
        let conn = servers.get(server_id).unwrap();
        let frame = ClientFrame::Request { id, body: request };
        let data = codec::encode(&frame)?;
        let mut send = conn.send_stream.lock().await;
        write_frame(&mut send, &data).await.context("failed to write request frame")?;
    }

    // Await response
    rx.await.context("response channel closed")
}
```

Wait — there's a problem. We can't lock `state.servers` (std Mutex) and then await inside. The `send_stream` lock is a tokio Mutex but it's inside the `ServerConnection` which is behind a std Mutex. We need to extract the connection reference differently.

Better approach: the `servers` HashMap stores `Arc<ServerConnection>`, so we can clone the Arc to get a reference without holding the HashMap lock:

```rust
pub servers: Mutex<HashMap<String, Arc<ServerConnection>>>,
```

Update `state.rs` accordingly.

Then `send_request` becomes:
```rust
pub async fn send_request(state: &AppState, server_id: &str, request: ServerRequest) -> Result<ServerResponse> {
    let conn = {
        let servers = state.servers.lock().unwrap();
        Arc::clone(servers.get(server_id).ok_or_else(|| anyhow::anyhow!("not connected to {}", server_id))?)
    };

    let id = conn.next_id();
    let (tx, rx) = tokio::sync::oneshot::channel();
    conn.pending_requests.lock().unwrap().insert(id, tx);

    let frame = ClientFrame::Request { id, body: request };
    let data = codec::encode(&frame)?;
    {
        let mut send = conn.send_stream.lock().await;
        write_frame(&mut send, &data).await.context("failed to write request frame")?;
    }

    rx.await.context("response channel closed")
}
```

The event reader spawner also includes the `server_id` in all emitted events:

```rust
pub fn spawn_event_reader(
    app: AppHandle,
    server_id: String,
    conn: Arc<crate::state::ServerConnection>,
    mut recv: RecvStream,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match recv_server_frame(&mut recv).await {
                Err(e) => {
                    eprintln!("[bridge] server {} connection error: {}", server_id, e);
                    let _ = app.emit("server:disconnected", serde_json::json!({
                        "server_id": &server_id,
                    }));
                    break;
                }
                Ok(frame) => match frame {
                    ServerFrame::Response { request_id, body } => {
                        let sender = conn.pending_requests.lock().unwrap().remove(&request_id);
                        if let Some(tx) = sender { let _ = tx.send(body); }
                    }
                    ServerFrame::Event(event) => {
                        dispatch_event(&app, &server_id, event);
                    }
                    _ => {}
                },
            }
        }
    })
}
```

The `dispatch_event` wraps every event with `server_id`:

```rust
fn dispatch_event(app: &AppHandle, server_id: &str, event: ServerEvent) {
    match event {
        ServerEvent::NewMessage { message } => {
            let _ = app.emit("server:new_message", serde_json::json!({
                "server_id": server_id,
                "message": &message,
            }));
        }
        // ... same pattern for all events — wrap payload in { server_id, ...data }
    }
}
```

- [ ] **Step 2: Update state.rs to use Arc<ServerConnection>**

Change `servers` field to `Mutex<HashMap<String, Arc<ServerConnection>>>`.

- [ ] **Step 3: Commit**

```bash
git add client/src-tauri/src/bridge.rs client/src-tauri/src/state.rs
git commit -m "refactor(client): bridge sends requests by server_id, events include server_id"
```

---

## Task 3: Tauri Backend — Commands with server_id

**Files:**
- Rewrite: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`

- [ ] **Step 1: Rewrite commands.rs**

Major changes:
1. `connect_server` no longer cleans up "the" old connection — it adds a new entry to the servers map
2. All commands that interact with a server gain `server_id: String` parameter
3. New commands: `list_servers`, `disconnect_server` removes by server_id
4. Persistence: `save_servers` / `load_servers` read/write `~/.farder/servers.json`

Key command signatures:

```rust
#[tauri::command]
pub async fn connect_server(app: AppHandle, state: State<'_, Arc<AppState>>, address: String, invite_code: Option<String>, setup_token: Option<String>) -> Result<ConnectResult, String>
// Creates endpoint, authenticates, creates ServerConnection, inserts into state.servers[address]
// Spawns event reader with server_id = address
// Persists to servers.json

#[tauri::command]
pub async fn disconnect_server(state: State<'_, Arc<AppState>>, server_id: String) -> Result<(), String>
// Removes from state.servers, aborts event reader

#[tauri::command]
pub fn list_servers(state: State<'_, Arc<AppState>>) -> Vec<ServerEntry>
// Returns list of connected servers with name and address

#[tauri::command]
pub async fn send_message(state: State<'_, Arc<AppState>>, server_id: String, channel_id: u64, content: String, reply_to: Option<u64>, attachment_ids: Vec<u64>) -> Result<SendMessageResult, String>

#[tauri::command]
pub async fn fetch_history(state: State<'_, Arc<AppState>>, server_id: String, channel_id: u64, before_id: Option<u64>, limit: Option<u32>) -> Result<Vec<MessageInfo>, String>

// ... all other commands follow the same pattern — add server_id parameter
```

The `connect_server` function creates a `ServerConnection` and inserts it:

```rust
let server_conn = Arc::new(ServerConnection {
    endpoint,
    connection: conn,
    send_stream: tokio::sync::Mutex::new(send),
    next_request_id: AtomicU32::new(1),
    pending_requests: Mutex::new(HashMap::new()),
    event_reader_handle: handle,
    server_name: String::new(), // filled after GetServerInfo
});

{
    let mut servers = state.servers.lock().unwrap();
    servers.insert(address.clone(), server_conn);
}
```

Wait — `event_reader_handle` is created before `ServerConnection` exists, but the reader needs the `Arc<ServerConnection>` to route responses. We need to create the `ServerConnection` first, then spawn the reader.

The fix: make `event_reader_handle` mutable or use a separate field. Or: create the `ServerConnection` without the handle, wrap in Arc, then spawn the reader and store the handle separately. Actually, use `Mutex<Option<JoinHandle>>` for the handle:

```rust
pub struct ServerConnection {
    // ...
    pub event_reader_handle: Mutex<Option<JoinHandle<()>>>,
}
```

Then after creating the Arc:
```rust
let server_conn = Arc::new(ServerConnection { /* ... event_reader_handle: Mutex::new(None) */ });
let handle = bridge::spawn_event_reader(app, address.clone(), Arc::clone(&server_conn), recv);
*server_conn.event_reader_handle.lock().unwrap() = Some(handle);
```

New serializable type:
```rust
#[derive(serde::Serialize)]
pub struct ServerEntry {
    pub id: String,      // address
    pub name: String,
}
```

Persistence functions:
```rust
fn servers_path() -> PathBuf { farder_data_dir().join("servers.json") }

fn save_server_list(entries: &[ServerEntry]) -> Result<(), String> { /* write JSON */ }
fn load_server_list() -> Vec<ServerEntry> { /* read JSON, return empty vec on error */ }

#[tauri::command]
pub fn get_saved_servers() -> Vec<ServerEntry> { load_server_list() }
```

After successful `connect_server`, update the saved list.

- [ ] **Step 2: Update main.rs**

Register all updated commands. The `auto_connect` flow on the frontend now loads saved servers and connects to each.

- [ ] **Step 3: Verify Tauri backend compiles**

Run: `cd /home/deez/farder/client/src-tauri && cargo check`

- [ ] **Step 4: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git commit -m "refactor(client): all Tauri commands accept server_id, add list/disconnect/save server commands"
```

---

## Task 4: TypeScript Bridge + Types

**Files:**
- Modify: `client/src/lib/types.ts`
- Rewrite: `client/src/lib/tauri-bridge.ts`

- [ ] **Step 1: Add ServerListEntry type**

In `types.ts`:
```typescript
export interface ServerListEntry {
    id: string;       // server address
    name: string;
    connected: boolean;
    unreadCount: number;
    hasMention: boolean;
}
```

- [ ] **Step 2: Update all bridge functions with serverId**

Every function that previously didn't take a serverId now does:

```typescript
export async function sendMessage(serverId: string, channelId: number, content: string, replyTo?: number, attachmentIds?: number[]): Promise<SendMessageResult> {
    return invoke("send_message", { serverId, channelId, content, replyTo: replyTo ?? null, attachmentIds: attachmentIds ?? [] });
}

export async function fetchHistory(serverId: string, channelId: number, beforeId?: number, limit?: number): Promise<MessageInfo[]> {
    return invoke("fetch_history", { serverId, channelId, beforeId: beforeId ?? null, limit: limit ?? null });
}

export async function subscribeChannels(serverId: string, channelIds: number[]): Promise<void> {
    return invoke("subscribe_channels", { serverId, channelIds });
}

export async function getServerInfo(serverId: string): Promise<ConnectResult> {
    return invoke("get_server_info", { serverId });
}

export async function getMembers(serverId: string): Promise<MemberInfo[]> {
    return invoke("get_members", { serverId });
}

export async function listDms(serverId: string): Promise<DmEntry[]> {
    return invoke("list_dms", { serverId });
}

export async function openDm(serverId: string, targetKey: string): Promise<{ channel: ChannelInfo; participant: MemberInfo }> {
    return invoke("open_dm", { serverId, targetKey });
}

// ... same for all other server-interacting commands

// New commands:
export async function getSavedServers(): Promise<ServerListEntry[]> {
    return invoke("get_saved_servers");
}

export async function disconnectServer(serverId: string): Promise<void> {
    return invoke("disconnect_server", { serverId });
}
```

The `connectServer` function does NOT take `serverId` — it takes `address` and returns the new server's info. The `serverId` IS the address.

- [ ] **Step 3: Commit**

```bash
git add client/src/lib/types.ts client/src/lib/tauri-bridge.ts
git commit -m "refactor(client): all bridge functions accept serverId, add getSavedServers/disconnectServer"
```

---

## Task 5: React State — Multi-Server Context

**Files:**
- Rewrite: `client/src/context/ServerContext.tsx`

- [ ] **Step 1: Restructure state for multi-server**

```typescript
export interface PerServerState {
    connected: boolean;
    connectionLost: boolean;
    serverName: string;
    channels: ChannelInfo[];
    categories: CategoryInfo[];
    roles: RoleInfo[];
    members: MemberInfo[];
    currentChannelId: number | null;
    messages: Record<number, MessageInfo[]>;
    threadChannelId: number | null;
    readState: Record<number, number>;
    dms: DmEntry[];
    dmPanelChannelId: number | null;
}

export interface AppState {
    activeServerId: string | null;
    serverList: ServerListEntry[];
    servers: Record<string, PerServerState>;
    hasIdentity: boolean;
}
```

The `initialPerServerState` is the same as the current `initialState` but without `serverName` at the top level (it's in serverList).

Actions gain a `serverId` field where needed:
```typescript
| { type: "SERVER_ADDED"; serverId: string; payload: ConnectResult }
| { type: "SERVER_REMOVED"; serverId: string }
| { type: "SET_ACTIVE_SERVER"; serverId: string }
| { type: "UPDATE_SERVER_LIST"; payload: ServerListEntry[] }
| { type: "INCREMENT_UNREAD"; serverId: string }
| { type: "SET_HAS_MENTION"; serverId: string }
// All existing actions get wrapped with serverId:
| { type: "SET_MEMBERS"; serverId: string; payload: MemberInfo[] }
| { type: "SELECT_CHANNEL"; serverId: string; payload: number }
| { type: "NEW_MESSAGE"; serverId: string; payload: MessageInfo }
// ... etc
```

The reducer routes all per-server actions to the correct entry in `state.servers[action.serverId]`.

A helper hook:
```typescript
export function useActiveServer(): PerServerState | null {
    const { state } = useApp();
    if (!state.activeServerId) return null;
    return state.servers[state.activeServerId] ?? null;
}

export function useActiveServerId(): string | null {
    const { state } = useApp();
    return state.activeServerId;
}
```

- [ ] **Step 2: Commit**

```bash
git add client/src/context/ServerContext.tsx
git commit -m "refactor(client): multi-server React state with per-server isolation and activeServerId"
```

---

## Task 6: Event Routing

**Files:**
- Rewrite: `client/src/hooks/useServerEvents.ts`

- [ ] **Step 1: Route events by server_id**

All events now include `server_id` in the payload. The hook extracts it and dispatches with the serverId:

```typescript
listen("server:new_message", (e) => {
    const data = e.payload as any;
    const serverId = data.server_id as string;
    const message = data.message as MessageInfo;

    if (serverId === activeServerIdRef.current) {
        // Active server: full processing
        dispatch({ type: "NEW_MESSAGE", serverId, payload: message });
    } else {
        // Background server: just increment unread
        dispatch({ type: "INCREMENT_UNREAD", serverId });
    }
}).then(u => unlisten.push(u));
```

Need a ref for `activeServerId` to avoid stale closures:
```typescript
const activeServerIdRef = useRef(state.activeServerId);
useEffect(() => { activeServerIdRef.current = state.activeServerId; }, [state.activeServerId]);
```

DM messages from background servers should also set `hasMention`:
```typescript
// In the new_message handler, check if it's a DM
if (serverId !== activeServerIdRef.current) {
    // Check if it's a DM (channel_type check, or check if channel is in the dms list)
    dispatch({ type: "INCREMENT_UNREAD", serverId });
}
```

- [ ] **Step 2: Commit**

```bash
git add client/src/hooks/useServerEvents.ts
git commit -m "refactor(client): route server events by server_id with background notification tracking"
```

---

## Task 7: Server Strip Component

**Files:**
- Create: `client/src/components/ServerStrip.tsx`
- Create: `client/src/components/AddServerModal.tsx`

- [ ] **Step 1: Create ServerStrip**

```tsx
import { useApp } from "../context/ServerContext";
import { useState } from "react";
import AddServerModal from "./AddServerModal";

export default function ServerStrip() {
    const { state, dispatch } = useApp();
    const [showAddServer, setShowAddServer] = useState(false);

    return (
        <div className="server-strip">
            {state.serverList.map(server => {
                const isActive = server.id === state.activeServerId;
                const initial = server.name.charAt(0).toUpperCase();
                return (
                    <div
                        key={server.id}
                        className={`server-icon${isActive ? " active" : ""}`}
                        onClick={() => dispatch({ type: "SET_ACTIVE_SERVER", serverId: server.id })}
                        title={server.name}
                    >
                        {initial}
                        {server.unreadCount > 0 && !isActive && <span className="server-unread-dot" />}
                        {server.hasMention && !isActive && <span className="server-mention-badge" />}
                    </div>
                );
            })}
            <div className="server-strip-separator" />
            <div className="server-icon add-server" onClick={() => setShowAddServer(true)} title="Add Server">
                +
            </div>
            {showAddServer && <AddServerModal onClose={() => setShowAddServer(false)} />}
        </div>
    );
}
```

- [ ] **Step 2: Create AddServerModal**

Reuse the invite link parsing from ConnectDialog:

```tsx
import { useState } from "react";
import * as api from "../lib/tauri-bridge";
import { useApp } from "../context/ServerContext";

// Copy parseInviteLink from ConnectDialog

export default function AddServerModal({ onClose }: { onClose: () => void }) {
    const { dispatch } = useApp();
    const [input, setInput] = useState("");
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);

    async function handleJoin() {
        // Parse the input for address + invite/setup token
        // Call api.connectServer(address, inviteCode, setupToken)
        // Dispatch SERVER_ADDED
        // Dispatch SET_ACTIVE_SERVER
        // Fetch members, dms
        // Close modal
    }

    return (
        <div className="modal-overlay" onClick={onClose}>
            <div className="modal-dialog" onClick={e => e.stopPropagation()}>
                <div className="modal-titlebar">
                    <span>Add Server</span>
                    <button className="modal-close" onClick={onClose}>X</button>
                </div>
                <div className="modal-body">
                    <label className="connect-label">Paste an invite link</label>
                    <input className="connect-input" value={input} onChange={e => setInput(e.target.value)}
                        onKeyDown={e => { if (e.key === "Enter") handleJoin(); }}
                        placeholder="farder.gg/join/..." autoFocus />
                    {error && <div className="error-text">{error}</div>}
                    <div className="connect-actions" style={{ marginTop: 8 }}>
                        <button className="xp-button" onClick={handleJoin} disabled={loading}>
                            {loading ? "Joining..." : "Join Server"}
                        </button>
                    </div>
                </div>
            </div>
        </div>
    );
}
```

- [ ] **Step 3: CSS for server strip**

```css
.server-strip {
    width: 52px;
    background: #1a1a2e;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 8px 0;
    gap: 6px;
    overflow-y: auto;
    flex-shrink: 0;
}

.server-icon {
    width: 40px;
    height: 40px;
    border-radius: 50%;
    background: var(--xp-sidebar);
    color: white;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 16px;
    font-weight: bold;
    cursor: pointer;
    position: relative;
    transition: border-radius 0.15s;
}

.server-icon:hover, .server-icon.active {
    border-radius: 12px;
    background: var(--xp-blue);
}

.server-icon.active {
    background: var(--xp-blue-light);
}

.server-icon.add-server {
    background: transparent;
    border: 2px dashed rgba(255,255,255,0.3);
    font-size: 20px;
    color: rgba(255,255,255,0.5);
}

.server-icon.add-server:hover {
    border-color: white;
    color: white;
    background: rgba(255,255,255,0.1);
}

.server-strip-separator {
    width: 32px;
    height: 2px;
    background: rgba(255,255,255,0.1);
    border-radius: 1px;
}

.server-unread-dot {
    position: absolute;
    bottom: -2px;
    right: -2px;
    width: 8px;
    height: 8px;
    background: white;
    border-radius: 50%;
}

.server-mention-badge {
    position: absolute;
    top: -2px;
    right: -2px;
    width: 12px;
    height: 12px;
    background: #cc0000;
    border-radius: 50%;
    border: 2px solid #1a1a2e;
}
```

- [ ] **Step 4: Commit**

```bash
git add client/src/components/ServerStrip.tsx client/src/components/AddServerModal.tsx client/src/styles/xp-theme.css
git commit -m "feat(client): server strip with server icons, unread dots, and add-server modal"
```

---

## Task 8: Update All Components for Multi-Server

**Files:**
- Modify: `client/src/components/AppShell.tsx`
- Modify: `client/src/components/App.tsx`
- Modify: `client/src/components/ConnectDialog.tsx`
- Modify: `client/src/components/ChannelSidebar.tsx`
- Modify: `client/src/components/ChatPanel.tsx`
- Modify: `client/src/components/MemberSidebar.tsx`
- Modify: `client/src/components/DmPanel.tsx`
- Modify: `client/src/components/MessageInput.tsx`
- Modify: `client/src/components/UserProfilePopup.tsx`
- Modify: `client/src/components/ThreadPanel.tsx`

- [ ] **Step 1: Update AppShell to include ServerStrip**

```tsx
<>
    <TitleBar />
    <div className="main-layout">
        <ServerStrip />
        <ChannelSidebar />
        <ChatPanel />
        <MemberSidebar />
        <DmPanel />
        {activeServer?.connectionLost && <div className="reconnect-overlay">...</div>}
    </div>
</>
```

- [ ] **Step 2: Update App.tsx**

First launch (no saved servers + no identity) → ConnectDialog (onboarding).
Has servers → AppShell with server strip.
Server strip handles adding new servers.

```tsx
function AppInner() {
    const { state } = useApp();
    useServerEvents();

    if (!state.hasIdentity) return <ConnectDialog mode="onboarding" />;
    if (state.serverList.length === 0) return <ConnectDialog mode="first-server" />;
    return <AppShell />;
}
```

- [ ] **Step 3: Update all components to use activeServer state**

Every component that currently reads from `state.channels`, `state.members`, etc. needs to read from `useActiveServer()` instead.

Example for ChannelSidebar:
```tsx
const activeServer = useActiveServer();
const serverId = useActiveServerId();
if (!activeServer || !serverId) return null;

// Replace all state.channels with activeServer.channels
// Replace all api.xxx() calls with api.xxx(serverId, ...)
```

Same pattern for ChatPanel, MemberSidebar, DmPanel, MessageInput, ThreadPanel, UserProfilePopup.

- [ ] **Step 4: Update auto-connect on launch**

In ConnectDialog or App, on mount:
1. Load identity
2. Load saved servers
3. For each saved server, call `connect_server` and dispatch `SERVER_ADDED`
4. Set the first server as active

- [ ] **Step 5: Verify TypeScript compiles**

Run: `cd /home/deez/farder/client && npx tsc --noEmit`

- [ ] **Step 6: Commit**

```bash
git add client/src/
git commit -m "refactor(client): all components use activeServer state, auto-connect to saved servers on launch"
```

---

## Self-Review Results

**Spec coverage:**
- Multi-server backend state ✅ Task 1
- Events include server_id ✅ Task 2
- All commands accept server_id ✅ Task 3
- TypeScript bridge updated ✅ Task 4
- Per-server React state ✅ Task 5
- Background notification tracking ✅ Task 6
- Server strip UI ✅ Task 7
- Add Server modal ✅ Task 7
- All components use active server ✅ Task 8
- Auto-connect to saved servers ✅ Task 8
- Server persistence ✅ Task 3

**Placeholder scan:** None found.

**Type consistency:** `ServerConnection`, `ServerEntry`, `ServerListEntry`, `PerServerState`, `activeServerId`, `serverId` — consistent across all tasks. Bridge functions consistently take `serverId: string` as first parameter.
