# Server Setup UX Redesign

## Problem

The current first-time server setup flow requires users to manually start the farder-server binary in a terminal, copy a hex setup token from the terminal output, and paste it into the client. This is hostile to non-technical users and unnecessary friction for everyone.

## Solution

Replace the current flow with a two-path choice screen and an integrated server setup wizard. The client bundles the server binary (Tauri sidecar) and manages the server process directly.

## Flow

### First-Time User

1. User opens app for the first time
2. "Welcome to Farder" — enter display name (existing flow, unchanged)
3. **New screen: "Create a Server" / "Join a Server"** — two buttons
4. User picks a path

### Returning User (Adding Another Server)

The "+" button in the server strip opens the same two-path choice (replaces current AddServerModal).

## Path A: "Create a Server"

### Wizard Steps

**Step 1 — Identity**
- Server name (text input, required)
- Server icon/avatar (optional, file picker)

**Step 2 — Configuration**
- Template picker: Gaming, Friends, Community, Blank (cards with descriptions)
- Privacy mode: Invite-only (default) / Open

### What Happens Under the Hood

1. Client locates the bundled `farder-server` sidecar binary
2. Picks an available port (scan from 4435 upward)
3. Creates a data directory at `~/.farder/servers/<server-name>/`
4. Spawns `farder-server --bind 0.0.0.0:<port> --name "<name>" --template <template> --db <datadir>/server.db --storage-dir <datadir>/files`
5. Waits for server to be ready (poll connection)
6. Connects and authenticates — server auto-claims first connection as owner (no setup token)
7. Registers the server process according to the selected management mode

### Server Process Management

Three modes, presented during setup with B as the recommended default:

**A) Embedded** — Server runs as a child process of the Tauri app. Starts and stops with the client. Simple, but friends can't connect when the app is closed.

**B) System Service (recommended)** — Server is registered as a system service (systemd on Linux, Windows Service / Task Scheduler on Windows, launchd on macOS). Runs independently of the client. Friends can connect anytime. Survives reboots.

**C) Background Process** — Client launches the server when opened. Server keeps running after the client closes. No auto-start on reboot. Middle ground between A and B.

All modes show a **system tray icon** in the notification area (near the clock) with:
- Server status (running/stopped)
- Connected user count
- Start/stop controls
- Open client button

## Path B: "Join a Server"

### UI

Single input field: "Paste an invite link"

Supported formats (parsed by existing `parseInviteLink` logic):
- `https://farder.gg/join/<base64>` — web invite link
- `farder://<host:port>/<code>` — deep link
- `<host:port>/<code>` — bare format

The "Advanced" section (manual address entry, public key display) is removed from the default view. If link parsing fails, the error message suggests checking the link format.

### Flow

1. User pastes invite link
2. Client parses address + invite code from the link
3. Connects, authenticates with invite code
4. Server appears in the server strip

## Server-Side Change: Auto-Claim Owner

### Current Behavior

Server generates a random setup token on first run, prints it to stdout. First user must provide this token to become owner.

### New Behavior

When the server has zero members, the first authenticated connection is automatically promoted to owner. No setup token required.

The setup token mechanism remains as a fallback for headless/remote server deployments where the admin starts the server manually and needs to designate an owner securely. But it is no longer the primary path.

### Implementation

In `auth.rs` / `connection.rs`: when authenticating a new member and `member_count == 0`, skip the invite/setup-token check and assign owner role directly.

## Tauri Sidecar Integration

The `farder-server` binary is bundled as a Tauri sidecar:
- Added to `tauri.conf.json` under `bundle.externalBin`
- Built alongside the client during `tauri build`
- Located at runtime via Tauri's sidecar resolution API
- Falls back to checking PATH for a user-installed `farder-server` (power user override)

## System Tray

A persistent tray icon (all platforms) provides:
- Tooltip showing server name + status
- Right-click menu: Start/Stop server, Open client, Quit
- Badge or color change to indicate running vs stopped

Implemented via Tauri's `tray-icon` plugin.

## Files Affected

### Server
- `crates/farder-server/src/auth.rs` — auto-claim owner when member_count == 0
- `crates/farder-server/src/connection.rs` — pass member count to auth flow
- `crates/farder-server/src/main.rs` — setup token becomes optional (still generated for headless use)

### Client (Tauri backend)
- `client/src-tauri/tauri.conf.json` — sidecar configuration
- `client/src-tauri/src/commands.rs` — new commands: `create_server`, `start_server`, `stop_server`, `get_server_status`
- `client/src-tauri/src/server_manager.rs` — new module: sidecar spawning, port selection, process lifecycle
- `client/src-tauri/src/tray.rs` — new module: system tray icon and menu
- `client/src-tauri/src/main.rs` — register tray, register new commands

### Client (React frontend)
- `client/src/components/ConnectDialog.tsx` — replace join screen with two-path choice, add "Create a Server" wizard
- `client/src/components/AddServerModal.tsx` — replace with same two-path choice
- `client/src/lib/tauri-bridge.ts` — add bindings for create_server, start_server, stop_server, get_server_status

## Out of Scope

- Server discovery / LAN broadcast
- Cloud-hosted server provisioning
- Multi-server management UI (beyond the tray icon)
- Server migration / backup tools
