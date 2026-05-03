# Server Setup UX Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the terminal-based server setup flow with a two-path UI ("Create a Server" / "Join a Server"), auto-claim owner on first connection, and manage the server process via Tauri sidecar with system tray integration.

**Architecture:** The farder-server binary is bundled as a Tauri sidecar. A new `server_manager` module handles spawning, port selection, and process lifecycle. The server auth layer gains auto-claim logic (first connection to an empty server becomes owner without tokens). The React frontend gets a two-step wizard for server creation and a simplified join flow.

**Tech Stack:** Rust (Tauri 2, Quinn/QUIC), React 18, TypeScript, Tauri sidecar API, tauri-plugin-shell (for sidecar), system tray via Tauri tray-icon API

---

### Task 1: Auto-Claim Owner on Empty Server (Server-Side)

**Files:**
- Modify: `crates/farder-server/src/auth.rs`
- Modify: `crates/farder-server/src/connection.rs:432-475`
- Modify: `crates/farder-server/src/main.rs:94-101`

This is the foundation — when a server has zero members, the first connection is auto-promoted to owner with no setup token or invite code required.

- [ ] **Step 1: Write the failing test for auto-claim**

Add to `crates/farder-server/src/auth.rs` at the end of the `mod tests` block (before the final `}`):

```rust
    #[test]
    fn test_auto_claim_owner_empty_server() {
        let conn = db::open_in_memory().unwrap();
        let keypair = Keypair::generate();
        let pk = keypair.public_key();

        // No invite, no setup token, but server is empty (0 members)
        let member_count: i64 = conn
            .query_row("SELECT count(*) FROM members", [], |row| row.get(0))
            .unwrap();
        assert_eq!(member_count, 0);

        let result = authenticate_new_member(
            &conn,
            &pk,
            "FirstUser",
            None,  // no invite
            None,  // no setup token hex
            None,  // no active setup token
        )
        .unwrap();

        assert!(result.is_ok(), "first connection to empty server should auto-claim: {:?}", result.err());

        let member = members::get_member(&conn, &pk).unwrap();
        assert!(member.is_some(), "member should be registered");
        assert_eq!(member.unwrap().display_name, "FirstUser");
    }

    #[test]
    fn test_no_auto_claim_when_members_exist() {
        let conn = db::open_in_memory().unwrap();

        // Register an existing member first
        let owner_kp = Keypair::generate();
        let owner_pk = owner_kp.public_key();
        members::register_member(&conn, &owner_pk, "Owner").unwrap();

        // Second user tries to connect with no invite/token
        let new_kp = Keypair::generate();
        let new_pk = new_kp.public_key();

        let result = authenticate_new_member(
            &conn,
            &new_pk,
            "Intruder",
            None,
            None,
            None,
        )
        .unwrap();

        assert!(result.is_err(), "should reject when members exist and no invite provided");
        assert_eq!(result.unwrap_err(), "no invite code or setup token provided");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p farder-server test_auto_claim_owner_empty_server test_no_auto_claim_when_members_exist -- --nocapture`
Expected: `test_auto_claim_owner_empty_server` FAILS (currently returns "no invite code or setup token provided"), `test_no_auto_claim_when_members_exist` PASSES.

- [ ] **Step 3: Implement auto-claim in authenticate_new_member**

In `crates/farder-server/src/auth.rs`, replace the final line of `authenticate_new_member` (line 94):

```rust
    Ok(Err("no invite code or setup token provided".to_string()))
```

with:

```rust
    // Auto-claim: if the server has zero members, the first connection becomes owner.
    let member_count: i64 = conn
        .query_row("SELECT count(*) FROM members", [], |row| row.get(0))?;
    if member_count == 0 {
        crate::members::register_member(conn, public_key, display_name)?;
        let everyone_id: Option<u64> = conn.query_row(
            "SELECT id FROM roles WHERE name = '@everyone' AND builtin = 1",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        ).ok();
        if let Some(eid) = everyone_id {
            crate::members::assign_role(conn, public_key, eid)?;
        }
        return Ok(Ok(()));
    }

    Ok(Err("no invite code or setup token provided".to_string()))
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p farder-server auth::tests -- --nocapture`
Expected: ALL auth tests pass, including both new ones.

- [ ] **Step 5: Update connection.rs to set owner on auto-claim**

In `crates/farder-server/src/connection.rs`, the owner assignment block (lines 471-475) currently only triggers when `setup_token_used`. It also needs to trigger on auto-claim. Replace lines 434-475:

Find the block starting with `let mut setup_token_used = false;` and ending with the owner assignment. Replace with:

```rust
    let mut setup_token_used = false;
    let mut is_first_member = false;
    let auth_result: Result<(), String> = {
        let conn_db = state.db.lock().unwrap();
        let existing = members::get_member(&conn_db, &public_key)?;
        if let Some(_member) = existing {
            match auth::authenticate_existing_member(&conn_db, &public_key)? {
                Ok(()) => Ok(()),
                Err(reason) => Err(reason),
            }
        } else {
            // Check if this is the first member (auto-claim path)
            let member_count: i64 = conn_db
                .query_row("SELECT count(*) FROM members", [], |row| row.get(0))?;
            is_first_member = member_count == 0;

            let display_name = format!("vk_{}", hex::encode(&pk_bytes[..4]));
            let active_setup_token = state.setup_token.lock().unwrap().clone();
            match auth::authenticate_new_member(
                &conn_db,
                &public_key,
                &display_name,
                invite_code.as_deref(),
                setup_token.as_deref(),
                active_setup_token.as_ref(),
            )? {
                Ok(()) => {
                    if setup_token.is_some() {
                        drop(conn_db);
                        let mut st = state.setup_token.lock().unwrap();
                        if st.is_some() {
                            *st = None;
                            setup_token_used = true;
                        }
                        drop(st);
                    }
                    Ok(())
                }
                Err(reason) => Err(reason),
            }
        }
    };

    // If the setup token was just consumed OR this is the first member (auto-claim),
    // set the owner.
    if (setup_token_used || is_first_member) && auth_result.is_ok() {
        let mut owner = state.owner.write().await;
        *owner = Some(public_key.clone());
    }
```

- [ ] **Step 6: Make setup token optional in main.rs**

In `crates/farder-server/src/main.rs`, update the first-run block (lines 94-101). The setup token should still be generated for headless/remote use, but log a note about auto-claim:

Replace lines 94-101:

```rust
    if first_run {
        let setup_token = auth::generate_setup_token();
        let setup_hex = hex::encode(&setup_token);
        info!("=== FIRST RUN ===");
        info!("The first user to connect will automatically become the server owner.");
        info!("For headless/remote setup, use this token instead: {}", setup_hex);
        *state.setup_token.lock().unwrap() = Some(setup_token);
    }
```

- [ ] **Step 7: Run full server test suite**

Run: `cargo test -p farder-server -- --nocapture`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/farder-server/src/auth.rs crates/farder-server/src/connection.rs crates/farder-server/src/main.rs
git commit -m "feat(server): auto-claim owner on first connection to empty server"
```

---

### Task 2: Server Manager Module (Tauri Backend)

**Files:**
- Create: `client/src-tauri/src/server_manager.rs`
- Modify: `client/src-tauri/src/main.rs`
- Modify: `client/src-tauri/Cargo.toml`
- Modify: `client/src-tauri/tauri.conf.json`

This module handles spawning the farder-server sidecar, selecting an available port, and managing the process lifecycle.

- [ ] **Step 1: Add tauri-plugin-shell dependency**

In `client/src-tauri/Cargo.toml`, add to `[dependencies]`:

```toml
tauri-plugin-shell = "2"
```

In `client/src-tauri/tauri.conf.json`, update to add the shell plugin with sidecar scope and the externalBin entry:

```json
{
  "productName": "Farder",
  "version": "0.1.0",
  "identifier": "com.farder.app",
  "build": {
    "frontendDist": "../dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build"
  },
  "app": {
    "windows": [{ "title": "Farder", "width": 1200, "height": 800 }],
    "security": { "csp": null }
  },
  "bundle": {
    "externalBin": ["binaries/farder-server"]
  },
  "plugins": {
    "shell": {
      "sidecar": true,
      "scope": [
        {
          "name": "binaries/farder-server",
          "sidecar": true,
          "args": true
        }
      ]
    }
  }
}
```

- [ ] **Step 2: Create the sidecar binary directory and symlink**

For development, create a symlink so Tauri can find the server binary:

```bash
mkdir -p client/src-tauri/binaries
```

Create a build script note: during dev, the sidecar binary needs to be named with the target triple. For example on Linux x86_64: `farder-server-x86_64-unknown-linux-gnu`. We'll create a helper script for this.

Create `client/src-tauri/binaries/copy-sidecar.sh`:

```bash
#!/bin/bash
# Copy the farder-server binary into the sidecar directory with the correct target triple name.
# Run from the repo root after building the server: cargo build -p farder-server
TARGET_TRIPLE=$(rustc -vV | grep host | cut -d' ' -f2)
cp target/debug/farder-server "client/src-tauri/binaries/farder-server-${TARGET_TRIPLE}"
echo "Copied sidecar binary for ${TARGET_TRIPLE}"
```

```bash
chmod +x client/src-tauri/binaries/copy-sidecar.sh
```

- [ ] **Step 3: Write server_manager.rs**

Create `client/src-tauri/src/server_manager.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};

/// Information about a managed (locally-spawned) server.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedServer {
    pub name: String,
    pub port: u16,
    pub data_dir: String,
    pub template: String,
    pub privacy: String, // "invite-only" or "open"
}

/// Tracks all locally-spawned server processes.
pub struct ServerProcesses {
    pub children: Mutex<HashMap<u16, (ManagedServer, CommandChild)>>,
}

impl ServerProcesses {
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }
}

/// Find an available TCP port starting from `start`.
fn find_available_port(start: u16) -> Option<u16> {
    (start..start + 100).find(|&port| {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    })
}

/// Resolve the data directory for a server. Creates it if needed.
fn server_data_dir(server_name: &str) -> PathBuf {
    let safe_name = server_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>();
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("farder")
        .join("servers")
        .join(&safe_name);
    let _ = std::fs::create_dir_all(&dir);
    let files_dir = dir.join("files");
    let _ = std::fs::create_dir_all(&files_dir);
    dir
}

/// Spawn a farder-server sidecar process with the given configuration.
pub fn spawn_server(
    app: &AppHandle,
    name: &str,
    template: &str,
    privacy: &str,
) -> Result<ManagedServer, String> {
    let port = find_available_port(4435)
        .ok_or_else(|| "no available port found (tried 4435-4534)".to_string())?;

    let data_dir = server_data_dir(name);
    let db_path = data_dir.join("server.db");
    let files_path = data_dir.join("files");

    let bind_addr = format!("0.0.0.0:{}", port);

    let sidecar = app
        .shell()
        .sidecar("binaries/farder-server")
        .map_err(|e| format!("failed to locate farder-server sidecar: {}", e))?
        .args([
            "--bind", &bind_addr,
            "--name", name,
            "--template", template,
            "--db", &db_path.to_string_lossy(),
            "--storage-dir", &files_path.to_string_lossy(),
        ]);

    let (mut rx, child) = sidecar
        .spawn()
        .map_err(|e| format!("failed to spawn farder-server: {}", e))?;

    // Spawn a task to read stdout/stderr so the pipe doesn't fill up
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    eprintln!("[farder-server] {}", String::from_utf8_lossy(&line));
                }
                CommandEvent::Stderr(line) => {
                    eprintln!("[farder-server] {}", String::from_utf8_lossy(&line));
                }
                CommandEvent::Terminated(status) => {
                    eprintln!("[farder-server] process exited: {:?}", status);
                    break;
                }
                _ => {}
            }
        }
    });

    let info = ManagedServer {
        name: name.to_string(),
        port,
        data_dir: data_dir.to_string_lossy().to_string(),
        template: template.to_string(),
        privacy: privacy.to_string(),
    };

    Ok(info)
}

/// Store the child process handle so we can stop it later.
pub fn register_child(
    procs: &ServerProcesses,
    info: ManagedServer,
    child: CommandChild,
) {
    let port = info.port;
    procs.children.lock().unwrap().insert(port, (info, child));
}

/// Stop a locally-managed server by port.
pub fn stop_server(procs: &ServerProcesses, port: u16) -> Result<(), String> {
    let mut children = procs.children.lock().unwrap();
    if let Some((_info, child)) = children.remove(&port) {
        child.kill().map_err(|e| format!("failed to kill server: {}", e))?;
    }
    Ok(())
}
```

- [ ] **Step 4: Register the shell plugin and server_manager module in main.rs**

In `client/src-tauri/src/main.rs`, add:

After the existing `mod tls;` line:
```rust
mod server_manager;
```

In the `tauri::Builder::default()` chain, add the shell plugin. Insert `.plugin(tauri_plugin_shell::init())` right after `.manage(Arc::new(AppState::new()))`:

```rust
    tauri::Builder::default()
        .manage(Arc::new(AppState::new()))
        .manage(server_manager::ServerProcesses::new())
        .plugin(tauri_plugin_shell::init())
        .setup(move |app| {
```

- [ ] **Step 5: Build to verify compilation**

First copy the sidecar binary for dev:
```bash
cd /home/deez/farder && cargo build -p farder-server && bash client/src-tauri/binaries/copy-sidecar.sh
```

Then build the client:
```bash
cd /home/deez/farder/client && npm run tauri build -- --debug 2>&1 | tail -20
```

Expected: Compiles without errors (may have warnings).

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/server_manager.rs client/src-tauri/src/main.rs client/src-tauri/Cargo.toml client/src-tauri/tauri.conf.json client/src-tauri/binaries/copy-sidecar.sh
git commit -m "feat(client): add server_manager module with sidecar spawning and port selection"
```

---

### Task 3: Tauri Commands for Server Creation & Management

**Files:**
- Modify: `client/src-tauri/src/commands.rs`
- Modify: `client/src-tauri/src/main.rs`

Expose the server manager functionality as Tauri commands that the React frontend can call.

- [ ] **Step 1: Add create_local_server command**

Add to the end of `client/src-tauri/src/commands.rs` (before any trailing `}`):

```rust
// ---------------------------------------------------------------------------
// Local server management commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn create_local_server(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    procs: State<'_, crate::server_manager::ServerProcesses>,
    name: String,
    template: String,
    privacy: String,
    icon_path: Option<String>,
) -> Result<serde_json::Value, String> {
    // Spawn the sidecar
    let info = crate::server_manager::spawn_server(&app, &name, &template, &privacy)?;
    let port = info.port;
    let address = format!("127.0.0.1:{}", port);

    // Wait for the server to be ready (poll up to 5 seconds)
    let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match crate::tls::make_client_endpoint() {
                Ok(endpoint) => {
                    let addr: std::net::SocketAddr = address.parse().unwrap();
                    if endpoint.connect(addr, "farder-server").is_ok() {
                        // Try the actual QUIC handshake
                        match endpoint.connect(addr, "farder-server").unwrap().await {
                            Ok(conn) => {
                                conn.close(0u32.into(), b"probe");
                                return true;
                            }
                            Err(_) => {}
                        }
                    }
                }
                Err(_) => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await;

    if ready.is_err() {
        return Err("server failed to start within 5 seconds".to_string());
    }

    // Now connect and auto-claim as owner (no invite or setup token needed)
    let keypair = {
        let ks = state.keypair.lock().unwrap();
        match &*ks {
            Some(bytes) => farder_crypto::identity::Keypair::from_signing_key_bytes(bytes),
            None => return Err("no identity keypair set".to_string()),
        }
    };

    let endpoint = crate::tls::make_client_endpoint().map_err(|e| e.to_string())?;
    let addr: std::net::SocketAddr = address.parse().map_err(|e: std::net::AddrParseError| e.to_string())?;

    let (conn, send, recv, session_token) =
        crate::connection::connect_and_authenticate(endpoint, addr, &keypair, None, None)
            .await
            .map_err(|e| e.to_string())?;

    // Store connection
    let server_conn = crate::state::ServerConnection {
        endpoint: crate::tls::make_client_endpoint().map_err(|e| e.to_string())?,
        connection: conn.clone(),
        session_token: session_token.clone(),
        server_name: std::sync::Mutex::new(name.clone()),
    };

    {
        let mut servers = state.servers.lock().unwrap();
        servers.insert(address.clone(), server_conn);
    }

    // Save to server list
    save_server_entry(&address, &name);

    // Spawn event reader
    crate::bridge::spawn_event_reader(app.clone(), address.clone(), conn.clone());

    // Set server avatar if provided
    if let Some(path) = icon_path {
        if let Ok(data) = std::fs::read(&path) {
            let dir = farder_data_dir().join("server_avatars");
            let _ = std::fs::create_dir_all(&dir);
            let safe_name = address.replace([':', '.', '/'], "_");
            let avatar_path = dir.join(format!("{}.png", safe_name));
            let _ = std::fs::write(&avatar_path, &data);
        }
    }

    // Fetch server info
    let response = crate::bridge::send_request(
        &state,
        &address,
        farder_protocol::server::ServerRequest::GetServerInfo,
    )
    .await
    .map_err(|e| e.to_string())?;

    match response {
        farder_protocol::server::ServerResponse::ServerInfo {
            name: srv_name,
            member_count,
            channels,
            categories,
            roles,
        } => Ok(serde_json::json!({
            "address": address,
            "server_name": srv_name,
            "member_count": member_count,
            "channels": channels,
            "categories": categories,
            "roles": roles,
        })),
        farder_protocol::server::ServerResponse::Error { reason } => Err(reason),
        other => Err(format!("unexpected response: {:?}", other)),
    }
}

#[tauri::command]
pub fn stop_local_server(
    procs: State<'_, crate::server_manager::ServerProcesses>,
    port: u16,
) -> Result<(), String> {
    crate::server_manager::stop_server(&procs, port)
}

#[tauri::command]
pub fn get_local_servers(
    procs: State<'_, crate::server_manager::ServerProcesses>,
) -> Vec<crate::server_manager::ManagedServer> {
    let children = procs.children.lock().unwrap();
    children.values().map(|(info, _)| info.clone()).collect()
}

#[tauri::command]
pub fn list_templates() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "id": "blank",
            "name": "Blank",
            "description": "Empty server — start from scratch"
        }),
        serde_json::json!({
            "id": "friend-group",
            "name": "Friends",
            "description": "Casual hangout for a small group of friends"
        }),
        serde_json::json!({
            "id": "gaming-community",
            "name": "Gaming",
            "description": "Voice lobbies, LFG, and game channels"
        }),
        serde_json::json!({
            "id": "organization",
            "name": "Organization",
            "description": "Teams, projects, and announcements"
        }),
        serde_json::json!({
            "id": "public-community",
            "name": "Community",
            "description": "Public community with moderation tools"
        }),
    ]
}
```

- [ ] **Step 2: Register the new commands in main.rs**

In `client/src-tauri/src/main.rs`, add to the `invoke_handler` list (before the closing `]`):

```rust
            commands::create_local_server,
            commands::stop_local_server,
            commands::get_local_servers,
            commands::list_templates,
```

- [ ] **Step 3: Build to verify compilation**

Run: `cd /home/deez/farder/client && cargo build --manifest-path src-tauri/Cargo.toml 2>&1 | tail -20`
Expected: Compiles (warnings OK).

- [ ] **Step 4: Commit**

```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs
git commit -m "feat(client): add Tauri commands for local server creation and management"
```

---

### Task 4: TypeScript API Bindings

**Files:**
- Modify: `client/src/lib/tauri-bridge.ts`

Add the frontend bindings for the new Tauri commands.

- [ ] **Step 1: Add server management bindings**

Add to the end of `client/src/lib/tauri-bridge.ts`:

```typescript
// ---------------------------------------------------------------------------
// Local server management
// ---------------------------------------------------------------------------

export interface TemplateInfo {
  id: string;
  name: string;
  description: string;
}

export interface ManagedServer {
  name: string;
  port: number;
  data_dir: string;
  template: string;
  privacy: string;
}

export async function createLocalServer(
  name: string,
  template: string,
  privacy: string,
  iconPath?: string,
): Promise<{
  address: string;
  server_name: string;
  member_count: number;
  channels: ChannelInfo[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
}> {
  return invoke("create_local_server", {
    name,
    template,
    privacy,
    iconPath: iconPath ?? null,
  });
}

export async function stopLocalServer(port: number): Promise<void> {
  return invoke("stop_local_server", { port });
}

export async function getLocalServers(): Promise<ManagedServer[]> {
  return invoke("get_local_servers");
}

export async function listTemplates(): Promise<TemplateInfo[]> {
  return invoke("list_templates");
}
```

- [ ] **Step 2: Verify the types ChannelInfo, CategoryInfo, RoleInfo already exist**

Check that these interfaces are already defined in `tauri-bridge.ts`. They are used by `connectServer` return type. If they are defined inline in the `connectServer` function, extract them. If they already exist as named exports, no action needed.

- [ ] **Step 3: Commit**

```bash
git add client/src/lib/tauri-bridge.ts
git commit -m "feat(client): add TypeScript bindings for local server management"
```

---

### Task 5: Two-Path Choice Screen (ConnectDialog)

**Files:**
- Modify: `client/src/components/ConnectDialog.tsx`
- Modify: `client/src/styles/xp-theme.css`

Replace the current "Join a Server" screen with a two-button choice: "Create a Server" / "Join a Server".

- [ ] **Step 1: Add CSS for the choice screen**

Add to the end of `client/src/styles/xp-theme.css`:

```css
/* ── Server Choice Screen ──────────────────────────────────── */
.server-choice {
  display: flex;
  gap: 16px;
  margin-top: 8px;
}

.server-choice-card {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 20px 16px;
  background: var(--xp-window-bg);
  border: 2px solid var(--xp-input-border);
  border-radius: 4px;
  cursor: pointer;
  transition: border-color 0.15s;
  text-align: center;
}

.server-choice-card:hover {
  border-color: var(--xp-blue);
  background: #e8f0fe;
}

.server-choice-card .choice-icon {
  font-size: 32px;
  color: var(--xp-blue-dark);
}

.server-choice-card .choice-title {
  font-weight: bold;
  font-size: 12px;
  color: var(--xp-blue-dark);
}

.server-choice-card .choice-desc {
  font-size: 11px;
  color: #666;
}

/* ── Create Server Wizard ──────────────────────────────────── */
.template-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 8px;
}

.template-card {
  padding: 10px;
  border: 2px solid var(--xp-input-border);
  border-radius: 4px;
  cursor: pointer;
  transition: border-color 0.15s;
}

.template-card:hover {
  border-color: var(--xp-blue);
}

.template-card.selected {
  border-color: var(--xp-blue);
  background: #e8f0fe;
}

.template-card .tmpl-name {
  font-weight: bold;
  font-size: 11px;
  color: var(--xp-blue-dark);
}

.template-card .tmpl-desc {
  font-size: 10px;
  color: #666;
  margin-top: 2px;
}

.privacy-options {
  display: flex;
  gap: 12px;
}

.privacy-option {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  cursor: pointer;
}
```

- [ ] **Step 2: Rewrite ConnectDialog with the new flow**

Replace the entire contents of `client/src/components/ConnectDialog.tsx`:

```tsx
import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useApp } from "../context/ServerContext";

type Step = "setup" | "choice" | "create-1" | "create-2" | "join";

function parseInviteLink(input: string): {
  address?: string;
  inviteCode?: string;
  setupToken?: string;
} {
  const trimmed = input.trim();
  if (!trimmed) return {};

  const joinMatch = trimmed.match(/(?:https?:\/\/)?farder\.gg\/join\/([A-Za-z0-9_-]+)/);
  if (joinMatch) {
    try {
      const decoded = atob(joinMatch[1].replace(/-/g, "+").replace(/_/g, "/"));
      const slashIdx = decoded.indexOf("/");
      if (slashIdx > 0) {
        const address = decoded.substring(0, slashIdx);
        const token = decoded.substring(slashIdx + 1);
        if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
        return { address, inviteCode: token };
      }
    } catch {}
  }

  const farderMatch = trimmed.match(/^farder:\/\/([^/]+)\/(.+)$/i);
  if (farderMatch) {
    const address = farderMatch[1];
    const token = farderMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  const slashMatch = trimmed.match(/^([^/]+:\d+)\/(.+)$/);
  if (slashMatch) {
    const address = slashMatch[1];
    const token = slashMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  if (/^[0-9a-f]{64}$/i.test(trimmed)) return { setupToken: trimmed };
  if (/^.+:\d+$/.test(trimmed)) return { address: trimmed };
  return { inviteCode: trimmed };
}

const DEFAULT_TEMPLATES = [
  { id: "blank", name: "Blank", description: "Empty server — start from scratch" },
  { id: "friend-group", name: "Friends", description: "Casual hangout for a small group" },
  { id: "gaming-community", name: "Gaming", description: "Voice lobbies, LFG, and game channels" },
  { id: "organization", name: "Organization", description: "Teams, projects, and announcements" },
  { id: "public-community", name: "Community", description: "Public community with moderation" },
];

export default function ConnectDialog() {
  const { dispatch } = useApp();

  const [step, setStep] = useState<Step>("setup");
  const [displayName, setDisplayName] = useState("");
  const [savedName, setSavedName] = useState<string | null>(null);
  const [pubKey, setPubKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Create server state
  const [serverName, setServerName] = useState("");
  const [serverIcon, setServerIcon] = useState<string | null>(null);
  const [selectedTemplate, setSelectedTemplate] = useState("blank");
  const [privacy, setPrivacy] = useState("invite-only");

  // Join server state
  const [inviteInput, setInviteInput] = useState("");

  useEffect(() => {
    async function init() {
      const [existingKey, existingName] = await Promise.allSettled([
        api.loadIdentity(),
        api.getDisplayName(),
      ]);
      const key = existingKey.status === "fulfilled" ? existingKey.value : null;
      const name = existingName.status === "fulfilled" ? existingName.value : null;
      if (key) setPubKey(key);
      if (key && name) {
        setSavedName(name);
        setStep("choice");
      }
    }
    init().catch(() => {});
  }, []);

  async function handleContinue() {
    const trimmed = displayName.trim();
    if (!trimmed) { setError("Please enter a display name."); return; }
    setLoading(true);
    setError(null);
    try {
      const key = await api.generateKeypair();
      await api.setDisplayName(trimmed);
      setPubKey(key);
      setSavedName(trimmed);
      dispatch({ type: "SET_IDENTITY" });
      setStep("choice");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function handlePickIcon() {
    try {
      const path = await api.pickFile();
      if (path) setServerIcon(path);
    } catch {}
  }

  async function handleCreateServer() {
    if (!serverName.trim()) { setError("Please enter a server name."); return; }
    setLoading(true);
    setError(null);
    try {
      const result = await api.createLocalServer(
        serverName.trim(),
        selectedTemplate,
        privacy,
        serverIcon ?? undefined,
      );
      dispatch({ type: "SERVER_ADDED", serverId: result.address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: result.address });
      try {
        const members = await api.getMembers(result.address);
        dispatch({ type: "SET_MEMBERS", serverId: result.address, payload: members });
      } catch {}
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function handleJoin() {
    setLoading(true);
    setError(null);

    if (!pubKey) {
      try {
        const key = await api.generateKeypair();
        setPubKey(key);
        dispatch({ type: "SET_IDENTITY" });
      } catch (e) {
        setError(String(e));
        setLoading(false);
        return;
      }
    }

    try {
      const parsed = parseInviteLink(inviteInput);
      const address = parsed.address;
      if (!address) {
        setError("Couldn't find a server address in that link. Try a farder.gg or farder:// link.");
        setLoading(false);
        return;
      }
      const result = await api.connectServer(address, parsed.inviteCode, parsed.setupToken);
      dispatch({ type: "SERVER_ADDED", serverId: address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: address });
      try {
        const members = await api.getMembers(address);
        dispatch({ type: "SET_MEMBERS", serverId: address, payload: members });
      } catch {}
      try {
        const dms = await api.listDms(address);
        dispatch({ type: "SET_DMS", serverId: address, payload: dms });
      } catch {}
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  // ── Step: Display Name ──────────────────────────────────────
  if (step === "setup") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Welcome to Farder</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">What should we call you?</div>
              <input
                className="connect-input"
                type="text"
                placeholder="Display name"
                value={displayName}
                maxLength={32}
                onChange={(e) => setDisplayName(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") handleContinue(); }}
                autoFocus
              />
            </div>
            {error && <div className="error-text">{error}</div>}
            <div className="connect-actions">
              <button className="xp-button" onClick={handleContinue} disabled={loading}>
                {loading ? "Setting up..." : "Continue"}
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // ── Step: Choice ────────────────────────────────────────────
  if (step === "choice") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Get Started</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">Hi, {savedName}!</div>
            </div>
            <div className="server-choice">
              <div
                className="server-choice-card"
                onClick={() => { setError(null); setStep("create-1"); }}
              >
                <div className="choice-icon">+</div>
                <div className="choice-title">Create a Server</div>
                <div className="choice-desc">
                  Start your own community.<br />You'll be the owner.
                </div>
              </div>
              <div
                className="server-choice-card"
                onClick={() => { setError(null); setStep("join"); }}
              >
                <div className="choice-icon">&#x1f517;</div>
                <div className="choice-title">Join a Server</div>
                <div className="choice-desc">
                  Got an invite link?<br />Paste it to connect.
                </div>
              </div>
            </div>
            <div className="connect-footer-links">
              <button className="connect-link" onClick={() => { setStep("setup"); setDisplayName(savedName ?? ""); setError(null); }}>
                Change display name
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // ── Step: Create Server — Name & Icon ───────────────────────
  if (step === "create-1") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Create a Server</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">Server Name</div>
              <input
                className="connect-input"
                type="text"
                placeholder="My Awesome Server"
                value={serverName}
                maxLength={64}
                onChange={(e) => setServerName(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter" && serverName.trim()) setStep("create-2"); }}
                autoFocus
              />
            </div>
            <div className="connect-section">
              <div className="connect-section-title">Server Icon (optional)</div>
              <button className="xp-button" onClick={handlePickIcon}>
                {serverIcon ? "Change icon..." : "Choose icon..."}
              </button>
              {serverIcon && <span style={{ fontSize: 10, color: "#666" }}>Selected</span>}
            </div>
            {error && <div className="error-text">{error}</div>}
            <div className="connect-actions">
              <button className="xp-button" onClick={() => { setError(null); setStep("choice"); }}>
                Back
              </button>
              <button
                className="xp-button"
                onClick={() => {
                  if (!serverName.trim()) { setError("Please enter a server name."); return; }
                  setError(null);
                  setStep("create-2");
                }}
              >
                Next
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // ── Step: Create Server — Template & Privacy ────────────────
  if (step === "create-2") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Create a Server</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">Choose a Template</div>
              <div className="template-grid">
                {DEFAULT_TEMPLATES.map((t) => (
                  <div
                    key={t.id}
                    className={`template-card${selectedTemplate === t.id ? " selected" : ""}`}
                    onClick={() => setSelectedTemplate(t.id)}
                  >
                    <div className="tmpl-name">{t.name}</div>
                    <div className="tmpl-desc">{t.description}</div>
                  </div>
                ))}
              </div>
            </div>
            <div className="connect-section">
              <div className="connect-section-title">Privacy</div>
              <div className="privacy-options">
                <label className="privacy-option">
                  <input
                    type="radio"
                    name="privacy"
                    checked={privacy === "invite-only"}
                    onChange={() => setPrivacy("invite-only")}
                  />
                  Invite only
                </label>
                <label className="privacy-option">
                  <input
                    type="radio"
                    name="privacy"
                    checked={privacy === "open"}
                    onChange={() => setPrivacy("open")}
                  />
                  Open
                </label>
              </div>
            </div>
            {error && <div className="error-text">{error}</div>}
            <div className="connect-actions">
              <button className="xp-button" onClick={() => { setError(null); setStep("create-1"); }}>
                Back
              </button>
              <button className="xp-button" onClick={handleCreateServer} disabled={loading}>
                {loading ? "Creating..." : "Create Server"}
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // ── Step: Join ──────────────────────────────────────────────
  return (
    <div className="connect-screen">
      <div className="connect-dialog">
        <div className="connect-dialog-titlebar">Join a Server</div>
        <div className="connect-dialog-body">
          <div className="connect-section">
            <div className="connect-section-title">Paste an invite link</div>
            <input
              className="connect-input"
              type="text"
              placeholder="farder.gg/join/... or farder://server/code"
              value={inviteInput}
              onChange={(e) => setInviteInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleJoin(); }}
              autoFocus
            />
          </div>
          {error && <div className="error-text">{error}</div>}
          <div className="connect-actions">
            <button className="xp-button" onClick={() => { setError(null); setStep("choice"); }}>
              Back
            </button>
            <button className="xp-button" onClick={handleJoin} disabled={loading}>
              {loading ? "Connecting..." : "Join"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Build frontend to verify**

Run: `cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | head -20`
Expected: No type errors.

- [ ] **Step 4: Commit**

```bash
git add client/src/components/ConnectDialog.tsx client/src/styles/xp-theme.css
git commit -m "feat(client): two-path choice screen with create server wizard"
```

---

### Task 6: Update AddServerModal with Two-Path Choice

**Files:**
- Modify: `client/src/components/AddServerModal.tsx`

The "+" button modal should offer the same Create/Join choice as the initial connect screen.

- [ ] **Step 1: Rewrite AddServerModal**

Replace the entire contents of `client/src/components/AddServerModal.tsx`:

```tsx
import { useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

type ModalStep = "choice" | "create-1" | "create-2" | "join";

function parseInviteLink(input: string): {
  address?: string;
  inviteCode?: string;
  setupToken?: string;
} {
  const trimmed = input.trim();
  if (!trimmed) return {};

  const joinMatch = trimmed.match(/(?:https?:\/\/)?farder\.gg\/join\/([A-Za-z0-9_-]+)/);
  if (joinMatch) {
    try {
      const decoded = atob(joinMatch[1].replace(/-/g, "+").replace(/_/g, "/"));
      const slashIdx = decoded.indexOf("/");
      if (slashIdx > 0) {
        const address = decoded.substring(0, slashIdx);
        const token = decoded.substring(slashIdx + 1);
        if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
        return { address, inviteCode: token };
      }
    } catch {}
  }

  const farderMatch = trimmed.match(/^farder:\/\/([^/]+)\/(.+)$/i);
  if (farderMatch) {
    const address = farderMatch[1];
    const token = farderMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  const slashMatch = trimmed.match(/^([^/]+:\d+)\/(.+)$/);
  if (slashMatch) {
    const address = slashMatch[1];
    const token = slashMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  if (/^[0-9a-f]{64}$/i.test(trimmed)) return { setupToken: trimmed };
  if (/^.+:\d+$/.test(trimmed)) return { address: trimmed };
  return { inviteCode: trimmed };
}

const DEFAULT_TEMPLATES = [
  { id: "blank", name: "Blank", description: "Empty server — start from scratch" },
  { id: "friend-group", name: "Friends", description: "Casual hangout for a small group" },
  { id: "gaming-community", name: "Gaming", description: "Voice lobbies, LFG, and game channels" },
  { id: "organization", name: "Organization", description: "Teams, projects, and announcements" },
  { id: "public-community", name: "Community", description: "Public community with moderation" },
];

export default function AddServerModal({ onClose }: { onClose: () => void }) {
  const { dispatch } = useApp();
  const [step, setStep] = useState<ModalStep>("choice");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Create state
  const [serverName, setServerName] = useState("");
  const [serverIcon, setServerIcon] = useState<string | null>(null);
  const [selectedTemplate, setSelectedTemplate] = useState("blank");
  const [privacy, setPrivacy] = useState("invite-only");

  // Join state
  const [inviteInput, setInviteInput] = useState("");

  async function handlePickIcon() {
    try {
      const path = await api.pickFile();
      if (path) setServerIcon(path);
    } catch {}
  }

  async function handleCreate() {
    if (!serverName.trim()) { setError("Please enter a server name."); return; }
    setLoading(true);
    setError(null);
    try {
      const result = await api.createLocalServer(
        serverName.trim(),
        selectedTemplate,
        privacy,
        serverIcon ?? undefined,
      );
      dispatch({ type: "SERVER_ADDED", serverId: result.address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: result.address });
      try {
        const members = await api.getMembers(result.address);
        dispatch({ type: "SET_MEMBERS", serverId: result.address, payload: members });
      } catch {}
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function handleJoin() {
    setLoading(true);
    setError(null);
    try {
      const parsed = parseInviteLink(inviteInput.trim());
      const address = parsed.address;
      if (!address) {
        setError("Couldn't find a server address in that link.");
        setLoading(false);
        return;
      }
      const result = await api.connectServer(address, parsed.inviteCode, parsed.setupToken);
      dispatch({ type: "SERVER_ADDED", serverId: address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: address });
      try {
        const members = await api.getMembers(address);
        dispatch({ type: "SET_MEMBERS", serverId: address, payload: members });
      } catch {}
      try {
        const dms = await api.listDms(address);
        dispatch({ type: "SET_DMS", serverId: address, payload: dms });
      } catch {}
      onClose();
    } catch (e) {
      setError(String(e));
    }
    setLoading(false);
  }

  function renderBody() {
    if (step === "choice") {
      return (
        <>
          <div className="server-choice">
            <div className="server-choice-card" onClick={() => { setError(null); setStep("create-1"); }}>
              <div className="choice-icon">+</div>
              <div className="choice-title">Create a Server</div>
              <div className="choice-desc">Start your own community.</div>
            </div>
            <div className="server-choice-card" onClick={() => { setError(null); setStep("join"); }}>
              <div className="choice-icon">&#x1f517;</div>
              <div className="choice-title">Join a Server</div>
              <div className="choice-desc">Paste an invite link.</div>
            </div>
          </div>
        </>
      );
    }

    if (step === "create-1") {
      return (
        <>
          <div className="connect-section">
            <div className="connect-section-title">Server Name</div>
            <input
              className="connect-input"
              type="text"
              placeholder="My Awesome Server"
              value={serverName}
              maxLength={64}
              onChange={(e) => setServerName(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && serverName.trim()) setStep("create-2"); }}
              autoFocus
            />
          </div>
          <div className="connect-section">
            <div className="connect-section-title">Server Icon (optional)</div>
            <button className="xp-button" onClick={handlePickIcon}>
              {serverIcon ? "Change icon..." : "Choose icon..."}
            </button>
          </div>
          {error && <div className="error-text">{error}</div>}
          <div className="connect-actions">
            <button className="xp-button" onClick={() => { setError(null); setStep("choice"); }}>Back</button>
            <button className="xp-button" onClick={() => {
              if (!serverName.trim()) { setError("Please enter a server name."); return; }
              setError(null); setStep("create-2");
            }}>Next</button>
          </div>
        </>
      );
    }

    if (step === "create-2") {
      return (
        <>
          <div className="connect-section">
            <div className="connect-section-title">Choose a Template</div>
            <div className="template-grid">
              {DEFAULT_TEMPLATES.map((t) => (
                <div
                  key={t.id}
                  className={`template-card${selectedTemplate === t.id ? " selected" : ""}`}
                  onClick={() => setSelectedTemplate(t.id)}
                >
                  <div className="tmpl-name">{t.name}</div>
                  <div className="tmpl-desc">{t.description}</div>
                </div>
              ))}
            </div>
          </div>
          <div className="connect-section">
            <div className="connect-section-title">Privacy</div>
            <div className="privacy-options">
              <label className="privacy-option">
                <input type="radio" name="modal-privacy" checked={privacy === "invite-only"} onChange={() => setPrivacy("invite-only")} />
                Invite only
              </label>
              <label className="privacy-option">
                <input type="radio" name="modal-privacy" checked={privacy === "open"} onChange={() => setPrivacy("open")} />
                Open
              </label>
            </div>
          </div>
          {error && <div className="error-text">{error}</div>}
          <div className="connect-actions">
            <button className="xp-button" onClick={() => { setError(null); setStep("create-1"); }}>Back</button>
            <button className="xp-button" onClick={handleCreate} disabled={loading}>
              {loading ? "Creating..." : "Create Server"}
            </button>
          </div>
        </>
      );
    }

    // join
    return (
      <>
        <div className="connect-section">
          <div className="connect-section-title">Paste an invite link</div>
          <input
            className="connect-input"
            type="text"
            placeholder="farder.gg/join/... or farder://server/code"
            value={inviteInput}
            onChange={(e) => setInviteInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") handleJoin(); }}
            autoFocus
          />
        </div>
        {error && <div className="error-text">{error}</div>}
        <div className="connect-actions">
          <button className="xp-button" onClick={() => { setError(null); setStep("choice"); }}>Back</button>
          <button className="xp-button" onClick={handleJoin} disabled={loading}>
            {loading ? "Joining..." : "Join Server"}
          </button>
        </div>
      </>
    );
  }

  const titleMap: Record<ModalStep, string> = {
    "choice": "Add Server",
    "create-1": "Create a Server",
    "create-2": "Create a Server",
    "join": "Join a Server",
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>{titleMap[step]}</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          {renderBody()}
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Build frontend to verify**

Run: `cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | head -20`
Expected: No type errors.

- [ ] **Step 3: Commit**

```bash
git add client/src/components/AddServerModal.tsx
git commit -m "feat(client): add two-path choice to AddServerModal"
```

---

### Task 7: System Tray Icon

**Files:**
- Create: `client/src-tauri/src/tray.rs`
- Modify: `client/src-tauri/src/main.rs`
- Modify: `client/src-tauri/Cargo.toml`

Add a system tray icon with server status and controls.

- [ ] **Step 1: Add tray dependency**

In `client/src-tauri/Cargo.toml`, add to `[dependencies]`:

```toml
tauri-plugin-notification = "2"
```

In `client/src-tauri/Cargo.toml`, update the tauri dependency to enable the tray-icon feature:

```toml
tauri = { version = "2", features = ["tray-icon"] }
```

- [ ] **Step 2: Create tray.rs**

Create `client/src-tauri/src/tray.rs`:

```rust
use tauri::{
    AppHandle,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
};

pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItemBuilder::with_id("show", "Open Farder").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&show)
        .separator()
        .item(&quit)
        .build()?;

    let _tray = TrayIconBuilder::new()
        .tooltip("Farder")
        .menu(&menu)
        .on_menu_event(move |app, event| {
            match event.id().as_ref() {
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "quit" => {
                    // Stop all managed servers before quitting
                    if let Some(procs) = app.try_state::<crate::server_manager::ServerProcesses>() {
                        let mut children = procs.children.lock().unwrap();
                        for (_port, (_info, child)) in children.drain() {
                            let _ = child.kill();
                        }
                    }
                    app.exit(0);
                }
                _ => {}
            }
        })
        .build(app)?;

    Ok(())
}
```

- [ ] **Step 3: Initialize tray in main.rs**

In `client/src-tauri/src/main.rs`, add after `mod server_manager;`:

```rust
mod tray;
```

In the `.setup(move |app| {` block, add tray initialization at the end (before `Ok(())`):

```rust
            if let Err(e) = tray::setup_tray(&app.handle()) {
                eprintln!("Failed to setup tray: {}", e);
            }
```

- [ ] **Step 4: Add the `get_webview_window` import**

In `client/src-tauri/src/tray.rs`, add at the top:

```rust
use tauri::Manager;
```

- [ ] **Step 5: Build to verify**

Run: `cd /home/deez/farder/client && cargo build --manifest-path src-tauri/Cargo.toml 2>&1 | tail -20`
Expected: Compiles.

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/tray.rs client/src-tauri/src/main.rs client/src-tauri/Cargo.toml
git commit -m "feat(client): system tray icon with Open/Quit controls"
```

---

### Task 8: Integration Test — Full Create Server Flow

**Files:**
- Modify: `tests/e2e_server.rs` (or create a new test)

End-to-end test: auto-claim on empty server works without any invite or setup token.

- [ ] **Step 1: Add auto-claim e2e test**

Add to `tests/e2e_server.rs` (or whichever file has the server e2e tests). Find the existing test pattern and add:

```rust
#[tokio::test]
async fn test_auto_claim_first_connection() {
    // Start a fresh server with no members
    let (endpoint, _server_task, _state) = start_test_server().await;

    // Connect without any invite or setup token
    let keypair = farder_crypto::identity::Keypair::generate();
    let client = connect_client(&endpoint, &keypair, None, None).await
        .expect("first connection should auto-claim owner");

    // Verify we're connected and can issue requests
    let info = send_request(&client, ServerRequest::GetServerInfo).await.unwrap();
    match info {
        ServerResponse::ServerInfo { member_count, .. } => {
            assert_eq!(member_count, 1, "should have exactly 1 member (the owner)");
        }
        other => panic!("expected ServerInfo, got {:?}", other),
    }

    // Second connection without invite should fail
    let keypair2 = farder_crypto::identity::Keypair::generate();
    let result = connect_client(&endpoint, &keypair2, None, None).await;
    assert!(result.is_err(), "second connection without invite should be rejected");
}
```

Note: Adapt `start_test_server`, `connect_client`, and `send_request` to match the existing test helper patterns in `tests/e2e_server.rs`. Read that file first and use the same helper functions.

- [ ] **Step 2: Run the test**

Run: `cargo test test_auto_claim_first_connection -- --nocapture`
Expected: PASSES.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e_server.rs
git commit -m "test: add e2e test for auto-claim owner on first connection"
```

---

### Task 9: Cleanup — Remove Debug Logging from Channel Creation

**Files:**
- Modify: `client/src/components/ServerSettingsDialog.tsx`

Remove the debug console.log statements that were added while investigating the channel creation bug.

- [ ] **Step 1: Remove debug logging**

In `client/src/components/ServerSettingsDialog.tsx`, find the `handleCreateChannel` function and revert to the clean version:

```typescript
  async function handleCreateChannel() {
    if (!newChName.trim() || !serverId) return;
    try {
      await api.createChannel(serverId, newChName.trim(), newChType, newChCatId);
      setNewChName("");
    } catch (e) { setError(String(e)); }
  }
```

- [ ] **Step 2: Commit**

```bash
git add client/src/components/ServerSettingsDialog.tsx
git commit -m "chore: remove debug logging from channel creation"
```
