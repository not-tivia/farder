# Farder Client v1: Server Chat UI — Design Spec

**Date:** 2026-04-03
**Status:** Draft
**Parent Spec:** `docs/specs/2026-04-01-privacy-chat-platform-design.md`
**Depends On:** Phase 2 (Servers & Text), Phase 3.1-3.3

## Goal

Build a desktop chat client that connects to a Farder server over QUIC, authenticates with Ed25519, and provides a Windows XP-themed interface for browsing channels, sending/receiving messages, reacting, and viewing threads. Single window, 3-panel layout (channel list | chat | member list).

## Tech Stack

- **Tauri 2** — desktop wrapper, Rust backend for QUIC/protocol
- **React 18** + **TypeScript 5** + **Vite** — frontend UI
- **Quinn 0.11** — QUIC client in the Tauri backend
- **farder-protocol** — MessagePack codec, server protocol types
- **farder-crypto** — Ed25519 identity, used for auth challenge-response
- **CSS** — custom XP Luna theme (no CSS framework)

## Architecture

### Tauri Rust Backend

The Tauri backend holds the QUIC connection and handles all protocol communication. React never touches the wire protocol directly.

**Connection state:**
```
struct ConnectionState {
    connection: Option<quinn::Connection>,
    main_send: Option<SendStream>,
    main_recv_task: Option<JoinHandle<()>>,  // background reader
    member_key: Option<PublicKey>,
    keypair: Option<Keypair>,
    is_owner: bool,
    next_request_id: AtomicU32,
    pending_requests: HashMap<u32, oneshot::Sender<ServerResponse>>,
}
```

**IPC Commands (Tauri → React):**

| Command | Args | Returns | Description |
|---------|------|---------|-------------|
| `generate_keypair` | — | `{ public_key: string }` | Generate and store Ed25519 keypair (existing) |
| `get_public_key` | — | `string \| null` | Get current public key (existing) |
| `connect_server` | `{ address, invite_code?, setup_token? }` | `{ server_info }` | Connect, authenticate, return server info |
| `disconnect_server` | — | — | Close QUIC connection |
| `send_message` | `{ channel_id, content, reply_to?, attachment_ids? }` | `{ id, timestamp }` | Send a message |
| `fetch_history` | `{ channel_id, before_id?, limit? }` | `{ messages }` | Fetch message history |
| `subscribe_channels` | `{ channel_ids }` | — | Subscribe to channel events |
| `get_server_info` | — | `{ name, channels, categories, roles, member_count }` | Refresh server info |
| `get_members` | — | `{ members }` | Get member list |
| `create_thread` | `{ message_id, name? }` | — | Create a thread |
| `add_reaction` | `{ message_id, emoji }` | — | Add a reaction |
| `remove_reaction` | `{ message_id, emoji }` | — | Remove own reaction |
| `request_deletion` | — | — | Request data deletion |
| `cancel_deletion` | — | — | Cancel pending deletion |
| `get_deletion_status` | — | `{ pending, requested_at?, expires_at? }` | Check deletion status |

**Tauri Events (Backend → React):**

| Event | Payload | Description |
|-------|---------|-------------|
| `server:new_message` | `MessageInfo` | New message in a subscribed channel |
| `server:message_edited` | `{ message_id, channel_id, new_content, edited_at }` | Message was edited |
| `server:message_deleted` | `{ message_id, channel_id }` | Message was deleted |
| `server:reaction_added` | `{ message_id, channel_id, emoji, public_key }` | Reaction added |
| `server:reaction_removed` | `{ message_id, channel_id, emoji, public_key }` | Reaction removed |
| `server:member_joined` | `{ public_key, display_name }` | Member joined |
| `server:member_left` | `{ public_key }` | Member left |
| `server:channel_created` | `ChannelInfo` | Channel/thread created |
| `server:channel_updated` | `ChannelInfo` | Channel updated |
| `server:channel_deleted` | `{ channel_id }` | Channel deleted |
| `server:typing` | `{ channel_id, public_key }` | Typing indicator |
| `server:disconnected` | `{ reason }` | Connection lost |

**Background reader task:** After auth, the backend spawns a tokio task that reads `ServerFrame` from the QUIC stream. For `ServerFrame::Response`, it matches the `request_id` to a pending oneshot sender and forwards the response. For `ServerFrame::Event`, it emits the corresponding Tauri event to the frontend.

**Request-response flow:**
1. React calls IPC command (e.g., `send_message`)
2. Tauri backend assigns a request ID, sends `ClientFrame::Request` on the QUIC stream
3. Backend creates a oneshot channel and stores the sender in `pending_requests[id]`
4. Background reader receives `ServerFrame::Response { request_id, body }`, finds the oneshot sender, sends the response
5. The IPC command handler awaits the oneshot receiver and returns the result to React

### React Frontend

**State management:** React Context + useReducer for global state (connected server info, current channel, messages, members). No external state library.

**Routing:** No router — single page with conditional rendering based on connection state:
- Not connected → `ConnectDialog`
- Connected → `AppShell` (3-panel layout)

### XP Luna Theme

Custom CSS mimicking Windows XP Luna Blue:

- **Title bar:** `linear-gradient(180deg, #0058E6 0%, #3389FF 50%, #0058E6 100%)`, white bold Tahoma text, min/max/close buttons
- **Window body:** `#ECE9D8` background
- **Sidebar:** Blue gradient (`#3169C6` → `#1941A5`), white text
- **Input fields:** White background, `#7F9DB9` border, inset shadow
- **Buttons:** XP classic raised style with `#ECE9D8` background, dark border
- **Scrollbars:** Styled to match XP (where CSS allows)
- **Font:** Tahoma 11px primary, 10px for secondary text
- **Channel list:** Tree-style with category headers in small caps
- **Member list:** Grouped by role, green/gray online indicators
- **Messages:** Author in bold colored text, timestamp in gray, content below

### Component Tree

```
App
├── ConnectDialog          (shown when not connected)
│   ├── IdentitySection    (generate/show keypair)
│   └── ServerForm         (address, invite code, setup token)
└── AppShell               (shown when connected)
    ├── TitleBar            (XP chrome: title, min/max/close)
    ├── MainLayout          (3-panel flex)
    │   ├── ChannelSidebar
    │   │   ├── ServerHeader      (server name)
    │   │   ├── CategoryList      (collapsible categories)
    │   │   │   └── ChannelItem   (name, unread badge, click handler)
    │   │   └── UserFooter        (identity, public key)
    │   ├── ChatPanel
    │   │   ├── ChannelHeader     (channel name, topic)
    │   │   ├── MessageList       (scrollable, auto-scroll)
    │   │   │   └── Message       (author, time, content, reactions, thread link, attachments)
    │   │   │       ├── ReactionBar      (reaction groups, add button)
    │   │   │       ├── AttachmentList   (inline previews / download links)
    │   │   │       └── ThreadLink       ("N replies" link)
    │   │   └── MessageInput      (text input, send button)
    │   └── MemberSidebar
    │       └── MemberList        (grouped by role, online dots)
    └── ThreadPanel         (replaces ChatPanel when viewing a thread)
```

### Data Flow

1. **Connect:** User enters server address + invite/setup token in `ConnectDialog`. React calls `connect_server` IPC. On success, receives server info (channels, categories, roles, members). Stores in context.
2. **Browse channels:** `CategoryList` renders from server info. Clicking a channel calls `subscribe_channels` and `fetch_history`. Messages stored in context keyed by channel_id.
3. **Send message:** User types in `MessageInput`, hits Enter. React calls `send_message` IPC. Optimistic update (show message immediately, confirm on `MessageSent` response).
4. **Receive events:** Background reader emits Tauri events. React listeners update context state (new messages, reactions, member joins/leaves).
5. **Reactions:** Click emoji on a message → show `ReactionPicker` → call `add_reaction`. Click existing reaction → toggle (add or remove).
6. **Threads:** Click "N replies" on a message → `ThreadPanel` replaces `ChatPanel`, subscribes to thread channel, fetches history. Back button returns to main channel.

## File Structure

### Tauri Backend (Rust)

```
client/src-tauri/src/
├── main.rs               # Tauri entry point, register all commands
├── state.rs              # AppState with ConnectionState (existing, extended)
├── commands.rs           # Identity commands (existing, extended with server commands)
├── connection.rs         # QUIC connection management, auth flow
├── bridge.rs             # Request-response dispatching, event forwarding
└── tls.rs                # SkipServerVerification (self-signed cert support)
```

### React Frontend (TypeScript)

```
client/src/
├── main.tsx              # React entry (existing)
├── App.tsx               # Root: ConnectDialog or AppShell (existing, rewritten)
├── context/
│   └── ServerContext.tsx  # Global state: connection, channels, messages, members
├── hooks/
│   ├── useServerEvents.ts    # Listen for Tauri events, dispatch to context
│   └── useTauriCommand.ts    # Typed wrapper for invoke()
├── components/
│   ├── ConnectDialog.tsx     # Server connection form + identity
│   ├── AppShell.tsx          # TitleBar + MainLayout container
│   ├── TitleBar.tsx          # XP window chrome
│   ├── ChannelSidebar.tsx    # Server header, category/channel tree, user footer
│   ├── ChatPanel.tsx         # Channel header, message list, input
│   ├── Message.tsx           # Single message with reactions, thread link, attachments
│   ├── MessageInput.tsx      # Text input with send
│   ├── ReactionPicker.tsx    # Emoji picker popup
│   ├── MemberSidebar.tsx     # Member list grouped by role
│   └── ThreadPanel.tsx       # Thread view (replaces ChatPanel)
├── styles/
│   ├── xp-theme.css          # XP Luna Blue global styles
│   ├── titlebar.css          # Title bar specific styles
│   ├── sidebar.css           # Channel/member sidebar styles
│   ├── chat.css              # Chat area styles
│   └── components.css        # Buttons, inputs, dialogs
└── lib/
    └── tauri-bridge.ts       # Typed IPC wrappers (existing, extended)
```

## What's NOT in Client v1

- DM / P2P messaging (separate sub-project)
- File upload UI (attachments display if present, no upload button)
- Settings / preferences panel
- Multiple simultaneous server connections
- Server admin features (role/channel management)
- Custom themes (XP Luna only)
- Notifications (system tray, desktop notifications)
- Message editing UI
- Emoji picker with full Unicode set (basic inline emoji only)
- Keyboard shortcuts
