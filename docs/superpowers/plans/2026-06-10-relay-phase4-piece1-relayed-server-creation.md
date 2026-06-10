# Relay Phase 4 piece 1 — In-App Relayed-Server Creation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user create a relayed server from the app — a local `farder-server` that dials the deployed default relay and registers, so it's reachable behind NAT with IPs hidden — with a relay-choice UX (Farder relay / self-host / direct).

**Architecture:** The client generates the `server_id`, pre-writes it to the server's data dir, spawns `farder-server --relay <addr> --data-dir <dir>` (no `--bind`), retries `connect_via_relay` until it registers, auto-claims owner, and saves the relay link as the server entry. Relayed servers respawn in relay mode on app relaunch (their relay-link id is stable). Direct servers are unchanged.

**Tech Stack:** Rust (Tauri 2, quinn), TypeScript/React. Live relay: `45.77.70.199:4433`.

**Spec:** `docs/superpowers/specs/2026-06-10-relay-phase4-piece1-relayed-server-creation-design.md`

---

## Context for the implementer

- **The client crate compiles with** `cd client/src-tauri && cargo build` (slow — Tauri deps). **Frontend type-checks with** `cd client && npx tsc --noEmit`. The app itself can't run here (WSL, no display) — do NOT attempt `npm run tauri dev`.
- A "server" is a local `farder-server` **child process** the client spawns (`server_manager.rs`). Direct mode binds a UDP port; relay mode dials out to the relay (no bind).
- The owner of a fresh server is **auto-claimed**: the first authenticated connection with **no invite** becomes owner. Over the relay, the owner connects with an **empty/None invite**.
- `DEFAULT_RELAY` (`client/src-tauri/src/default_relay.rs`) is now `Some {addr:"45.77.70.199:4433", cert_fp_hex:"7e3ed9b3…"}`.
- Reusable: `connect_via_relay`, `RelayTarget`, `build_relay_link`, `parse_relay_target` (`connection.rs`); `make_pinned_relay_endpoint` (`tls.rs`); server `--relay`/`--data-dir` flags + `load_or_generate_server_id` (writes/reads `<data_dir>/server_id`).
- Tauri seam: **no new commands** — `create_local_server` (already in `generate_handler!`) only gains params.
- Much of this can't be runtime-verified here. Gates: `cargo build`, `npx tsc --noEmit`, the seam, and **unit tests on the pure pieces**. The full create→register→join flow is the user's Windows run against the live relay — flag UNVERIFIED but note it's genuinely runnable now.

---

## File structure

- `client/src-tauri/src/default_relay.rs` — `default_relay()` accessor (testable parse).
- `client/src-tauri/src/server_manager.rs` — `ServerMode`, `build_server_args`, relay-aware `spawn_server` + `spawn_server_with_data_dir`, `ManagedServer.relayed`, `server_id` pre-write.
- `client/src-tauri/src/connection.rs` — empty-token handling (`parse_relay_target` + `connect_via_relay`).
- `client/src-tauri/src/commands.rs` — `resolve_relay_choice` + `create_local_server` relay branch + `restart_local_servers` relayed respawn.
- `client/src/components/AddServerModal.tsx` + `client/src/lib/tauri-bridge.ts` — relay-choice UX + params.
- `docs/modules/client-relay.md` — document in-app relayed-server creation.

---

## Task 1: `default_relay()` accessor

**Files:** Modify `client/src-tauri/src/default_relay.rs`.

- [ ] **Step 1: Write the failing test**

Add to `default_relay.rs` (create a `#[cfg(test)] mod tests` at the end):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_addr_and_fingerprint() {
        let got = parse_relay_config("45.77.70.199:4433", "7e3ed9b35aedcf3b42c30500720ca12cb1385ad0a74207b3f977167f1ab48459");
        let (addr, fp) = got.expect("should parse");
        assert_eq!(addr.port(), 4433);
        assert_eq!(fp.len(), 32);
    }

    #[test]
    fn rejects_bad_addr_or_fingerprint() {
        assert!(parse_relay_config("not-an-addr", "7e3ed9b3").is_none());
        assert!(parse_relay_config("45.77.70.199:4433", "zzzz").is_none());
        assert!(parse_relay_config("45.77.70.199:4433", "7e3e").is_none()); // not 32 bytes
    }

    #[test]
    fn default_relay_is_configured() {
        // The deployed default is filled in; it must parse.
        assert!(default_relay().is_some());
    }
}
```

- [ ] **Step 2: Run it to verify failure**

Run: `cd client/src-tauri && cargo test default_relay 2>&1 | tail -20`
Expected: FAIL to compile — `parse_relay_config` / `default_relay` don't exist.

- [ ] **Step 3: Implement the accessor**

In `default_relay.rs`: add `use std::net::SocketAddr;` at the top, drop the `#[allow(dead_code)]` on `DEFAULT_RELAY` and the struct, and add:

```rust
/// Parse a relay address + hex fingerprint into a (SocketAddr, 32-byte fp).
/// Returns None if the address is unparseable or the fingerprint isn't 32 hex bytes.
fn parse_relay_config(addr: &str, fp_hex: &str) -> Option<(SocketAddr, Vec<u8>)> {
    let addr: SocketAddr = addr.parse().ok()?;
    let fp = hex::decode(fp_hex).ok()?;
    if fp.len() != 32 {
        return None;
    }
    Some((addr, fp))
}

/// The configured default relay as a parsed (SocketAddr, cert fingerprint), or
/// None if no default relay is configured (or it is malformed).
pub fn default_relay() -> Option<(SocketAddr, Vec<u8>)> {
    let r = DEFAULT_RELAY.as_ref()?;
    parse_relay_config(r.addr, r.cert_fp_hex)
}
```

(Keep the `struct DefaultRelay` and the `const DEFAULT_RELAY` as-is, just without the `#[allow(dead_code)]` attributes.)

- [ ] **Step 4: Run the tests**

Run: `cd client/src-tauri && cargo test default_relay 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add client/src-tauri/src/default_relay.rs
git commit -m "Client: default_relay() accessor parses the configured relay (Phase 4 piece 1)"
```

---

## Task 2: Relay-aware server spawn

**Files:** Modify `client/src-tauri/src/server_manager.rs`.

- [ ] **Step 1: Write the failing test**

Add to `server_manager.rs` a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::path::Path;

    #[test]
    fn relay_mode_args_use_relay_and_data_dir_not_bind() {
        let addr: SocketAddr = "45.77.70.199:4433".parse().unwrap();
        let args = build_server_args("MyServer", "blank", Path::new("/tmp/s"), &ServerMode::Relay { relay_addr: addr });
        assert!(args.iter().any(|a| a == "--relay"));
        assert!(args.iter().any(|a| a == "45.77.70.199:4433"));
        assert!(args.iter().any(|a| a == "--data-dir"));
        assert!(!args.iter().any(|a| a == "--bind"), "relay mode must not bind a port");
    }

    #[test]
    fn direct_mode_args_bind_a_port() {
        let args = build_server_args("MyServer", "blank", Path::new("/tmp/s"), &ServerMode::Direct { port: 4435 });
        assert!(args.iter().any(|a| a == "--bind"));
        assert!(args.iter().any(|a| a == "0.0.0.0:4435"));
        assert!(!args.iter().any(|a| a == "--relay"));
    }
}
```

- [ ] **Step 2: Run it to verify failure**

Run: `cd client/src-tauri && cargo test server_manager 2>&1 | tail -20`
Expected: FAIL — `ServerMode` / `build_server_args` don't exist.

- [ ] **Step 3: Add `ServerMode` + `build_server_args` + the `relayed` field**

At the top of `server_manager.rs` add `use std::net::SocketAddr;`. Add the `relayed` field to `ManagedServer` (after `privacy`):

```rust
    pub privacy: String, // "invite-only" or "open"
    #[serde(default)]
    pub relayed: bool,
```

Add the mode enum + arg builder (after the `ManagedServer` struct):

```rust
/// How a spawned server is reached.
pub enum ServerMode {
    /// Bind a public UDP listener on `port`.
    Direct { port: u16 },
    /// Dial out to `relay_addr` and register (no local bind); the stable
    /// server_id lives in the data dir.
    Relay { relay_addr: SocketAddr },
}

/// Build the farder-server CLI args for the given mode. Pure (testable).
pub fn build_server_args(name: &str, template: &str, data_dir: &std::path::Path, mode: &ServerMode) -> Vec<String> {
    let db = data_dir.join("server.db").to_string_lossy().into_owned();
    let files = data_dir.join("files").to_string_lossy().into_owned();
    let mut args = vec![
        "--name".into(), name.to_string(),
        "--template".into(), template.to_string(),
        "--db".into(), db,
        "--storage-dir".into(), files,
    ];
    match mode {
        ServerMode::Direct { port } => {
            args.push("--bind".into());
            args.push(format!("0.0.0.0:{}", port));
        }
        ServerMode::Relay { relay_addr } => {
            args.push("--relay".into());
            args.push(relay_addr.to_string());
            args.push("--data-dir".into());
            args.push(data_dir.to_string_lossy().into_owned());
        }
    }
    args
}
```

- [ ] **Step 4: Make `spawn_server` relay-aware**

Replace `spawn_server` with:

```rust
/// Spawn a farder-server process. `relay` = Some((relay_addr, server_id)) starts
/// it in relay mode (writes the server_id, dials the relay, no bind); None is a
/// direct server bound to a local port.
pub fn spawn_server(
    name: &str,
    template: &str,
    _privacy: &str,
    relay: Option<(SocketAddr, [u8; 32])>,
) -> Result<(ManagedServer, Child), String> {
    // A unique port number is always allocated: it is the bind port for direct
    // servers, and just a process-table handle for relay servers (which don't bind).
    let port = find_available_port(4435)
        .ok_or_else(|| "no available port found (tried 4435-4534)".to_string())?;
    let data_dir = server_data_dir(name)?;
    let server_bin = find_server_binary()?;

    let mode = match relay {
        Some((relay_addr, server_id)) => {
            std::fs::write(data_dir.join("server_id"), server_id)
                .map_err(|e| format!("failed to write server_id: {}", e))?;
            ServerMode::Relay { relay_addr }
        }
        None => ServerMode::Direct { port },
    };
    let args = build_server_args(name, template, &data_dir, &mode);

    let child = Command::new(&server_bin)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn farder-server at {:?}: {}", server_bin, e))?;

    let info = ManagedServer {
        name: name.to_string(),
        port,
        data_dir: data_dir.to_string_lossy().to_string(),
        template: template.to_string(),
        privacy: _privacy.to_string(),
        relayed: relay.is_some(),
    };
    Ok((info, child))
}
```

- [ ] **Step 5: Make `spawn_server_with_data_dir` relay-aware**

Replace `spawn_server_with_data_dir` with (the server_id already exists in the data dir for a relayed respawn, so relay mode just needs the address):

```rust
/// Spawn a server using an existing data directory (for restarting on app
/// relaunch). `relay_addr` = Some(addr) respawns in relay mode (reuses the
/// existing server_id in the data dir); None is a direct server.
pub fn spawn_server_with_data_dir(
    name: &str,
    template: &str,
    data_dir: &str,
    relay_addr: Option<SocketAddr>,
) -> Result<(ManagedServer, Child), String> {
    let port = find_available_port(4435)
        .ok_or_else(|| "no available port found (tried 4435-4534)".to_string())?;
    let data_path = PathBuf::from(data_dir);
    let server_bin = find_server_binary()?;

    let mode = match relay_addr {
        Some(addr) => ServerMode::Relay { relay_addr: addr },
        None => ServerMode::Direct { port },
    };
    let args = build_server_args(name, template, &data_path, &mode);

    let child = Command::new(&server_bin)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn farder-server at {:?}: {}", server_bin, e))?;

    let info = ManagedServer {
        name: name.to_string(),
        port,
        data_dir: data_dir.to_string(),
        template: template.to_string(),
        privacy: "invite-only".to_string(),
        relayed: relay_addr.is_some(),
    };
    Ok((info, child))
}
```

- [ ] **Step 6: Run the tests**

Run: `cd client/src-tauri && cargo test server_manager 2>&1 | tail -20`
Expected: PASS (2 tests). (Callers in `commands.rs` won't compile yet — that's Tasks 4-5. If `cargo test` fails to build the whole crate because of the changed `spawn_server` signature at the call sites, that's expected; the unit test of `build_server_args` still demonstrates the logic. To get a clean test run now, you may temporarily update the two call sites in `commands.rs:2306` and `:2568` to pass `None` — Tasks 4-5 finalize them. Note that as DONE_WITH_CONCERNS if you do.)

- [ ] **Step 7: Commit**

```bash
git add client/src-tauri/src/server_manager.rs client/src-tauri/src/commands.rs
git commit -m "Client: relay-aware server spawn (ServerMode + build_server_args) (Phase 4 piece 1)"
```

---

## Task 3: Empty invite token over the relay (owner connect)

**Files:** Modify `client/src-tauri/src/connection.rs`.

- [ ] **Step 1: Write the failing test**

In `connection.rs`'s `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn relay_link_round_trips_an_empty_owner_token() {
        let addr: std::net::SocketAddr = "45.77.70.199:4433".parse().unwrap();
        let target = RelayTarget {
            relay_addr: addr,
            server_id: vec![1u8; 32],
            cert_fp: vec![2u8; 32],
            invite_token: String::new(),
        };
        let link = build_relay_link(&target, ""); // owner link: empty token
        let parsed = parse_relay_target(&link).expect("empty-token link must parse");
        assert_eq!(parsed.relay_addr, addr);
        assert_eq!(parsed.server_id, vec![1u8; 32]);
        assert_eq!(parsed.cert_fp, vec![2u8; 32]);
        assert!(parsed.invite_token.is_empty());
    }
```

- [ ] **Step 2: Run it to verify failure**

Run: `cd client/src-tauri && cargo test relay_link_round_trips 2>&1 | tail -20`
Expected: FAIL — `parse_relay_target` currently returns `None` for an empty token (the `parts[3].is_empty()` reject).

- [ ] **Step 3: Allow an empty token in `parse_relay_target`**

In `connection.rs`, change the reject condition (the `if server_id.is_empty() || cert_fp.is_empty() || parts[3].is_empty()` line) to drop the token check:

```rust
    if server_id.is_empty() || cert_fp.is_empty() {
        return None;
    }
```

(An empty token is valid only for the owner's own entry; a shared link with an empty token simply won't authenticate a new joiner, which is correct.)

- [ ] **Step 4: Map empty token to `None` invite in `connect_via_relay`**

In `connect_via_relay`, replace the `run_client_handshake(...)` invite argument. Change:

```rust
    let session_token = run_client_handshake(
        &mut send,
        &mut recv,
        keypair,
        Some(target.invite_token.clone()),
        setup_token,
    )
    .await?;
```
to:

```rust
    // An empty token means "no invite" — the owner auto-claims a fresh server.
    let invite = if target.invite_token.is_empty() {
        None
    } else {
        Some(target.invite_token.clone())
    };
    let session_token = run_client_handshake(&mut send, &mut recv, keypair, invite, setup_token).await?;
```

- [ ] **Step 5: Run the tests**

Run: `cd client/src-tauri && cargo test -p farder-client relay 2>&1 | tail -20` (or `cargo test relay_link_round_trips`)
Expected: PASS, and the existing `relay_it` / `parse_relay_target` tests still pass.

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/connection.rs
git commit -m "Client: relay links carry an empty owner invite token (Phase 4 piece 1)"
```

---

## Task 4: `create_local_server` relay branch

**Files:** Modify `client/src-tauri/src/commands.rs`.

- [ ] **Step 1: Write the failing test for the choice resolver**

In `commands.rs`, add (or extend) a `#[cfg(test)] mod tests` with:

```rust
#[cfg(test)]
mod relay_choice_tests {
    use super::*;

    #[test]
    fn direct_resolves_to_none() {
        assert!(resolve_relay_choice("direct", None, None).unwrap().is_none());
    }

    #[test]
    fn farder_resolves_to_the_default_relay() {
        let r = resolve_relay_choice("farder", None, None).unwrap();
        assert!(r.is_some(), "default relay is configured");
    }

    #[test]
    fn selfhost_validates_addr_and_fingerprint() {
        let ok = resolve_relay_choice("selfhost", Some("1.2.3.4:4433"), Some(&"ab".repeat(32))).unwrap();
        assert!(ok.is_some());
        assert!(resolve_relay_choice("selfhost", Some("nope"), Some(&"ab".repeat(32))).is_err());
        assert!(resolve_relay_choice("selfhost", Some("1.2.3.4:4433"), Some("zz")).is_err());
        assert!(resolve_relay_choice("selfhost", Some("1.2.3.4:4433"), Some("abcd")).is_err()); // not 32 bytes
    }
}
```

- [ ] **Step 2: Run it to verify failure**

Run: `cd client/src-tauri && cargo test relay_choice 2>&1 | tail -20`
Expected: FAIL — `resolve_relay_choice` doesn't exist.

- [ ] **Step 3: Add the resolver**

In `commands.rs`, add a module function near `create_local_server`:

```rust
/// Resolve the create-server relay choice into an optional (relay addr, cert fp).
/// `None` means a direct server. Validates self-host inputs.
fn resolve_relay_choice(
    mode: &str,
    addr: Option<&str>,
    fp: Option<&str>,
) -> Result<Option<(std::net::SocketAddr, Vec<u8>)>, String> {
    match mode {
        "direct" => Ok(None),
        "farder" => crate::default_relay::default_relay()
            .map(Some)
            .ok_or_else(|| "the Farder default relay is not configured in this build".to_string()),
        "selfhost" => {
            let addr = addr.unwrap_or("").trim();
            let fp = fp.unwrap_or("").trim();
            let sock: std::net::SocketAddr = addr
                .parse()
                .map_err(|_| format!("invalid relay address '{}' (expected host:port)", addr))?;
            let fp_bytes = hex::decode(fp).map_err(|_| "relay fingerprint must be hexadecimal".to_string())?;
            if fp_bytes.len() != 32 {
                return Err(format!(
                    "relay fingerprint must be 64 hex characters (32 bytes); got {} bytes",
                    fp_bytes.len()
                ));
            }
            Ok(Some((sock, fp_bytes)))
        }
        other => Err(format!("unknown relay mode '{}'", other)),
    }
}
```

- [ ] **Step 4: Run the resolver tests**

Run: `cd client/src-tauri && cargo test relay_choice 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 5: Branch `create_local_server`**

Add the three relay params to the command signature (after `icon_path`):

```rust
    icon_path: Option<String>,
    relay_mode: String,            // "farder" | "selfhost" | "direct"
    relay_addr: Option<String>,    // self-host only
    relay_fp: Option<String>,      // self-host only
```

After the existing name-trim / duplicate checks, resolve the choice and spawn accordingly. Replace the block from `// Spawn the server process` through the connect/probe (the part that produces `(conn, send, recv, session_token)` and decides the `address`) with:

```rust
    let relay = resolve_relay_choice(&relay_mode, relay_addr.as_deref(), relay_fp.as_deref())?;

    // Load the owner keypair up front (needed for both paths).
    let keypair = {
        let lock = state.signing_key_bytes.lock().map_err(|e| e.to_string())?;
        match lock.as_ref() {
            Some(bytes) => Keypair::from_signing_key_bytes(bytes),
            None => return Err("no identity keypair set — unlock your identity first".to_string()),
        }
    };

    // Generate a stable server_id (used only in relay mode).
    let server_id: [u8; 32] = rand::random();

    let (info, child) = crate::server_manager::spawn_server(
        &name,
        &template,
        &privacy,
        relay.as_ref().map(|(a, _)| (*a, server_id)),
    )?;
    let port = info.port;
    let relayed = info.relayed;
    let local_data_dir = info.data_dir.clone();
    let local_template = info.template.clone();
    procs.register(info, child);

    // Connect + obtain the entry id (relay link or 127.0.0.1:port).
    let (conn, send, recv, session_token, address, endpoint) = if let Some((relay_addr, cert_fp)) = relay {
        let target = crate::connection::RelayTarget {
            relay_addr,
            server_id: server_id.to_vec(),
            cert_fp: cert_fp.clone(),
            invite_token: String::new(), // owner: no invite
        };
        let endpoint = crate::tls::make_pinned_relay_endpoint(cert_fp.clone()).map_err(|e| e.to_string())?;
        // Retry until the server has registered with the relay (or time out).
        let connected = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                match crate::connection::connect_via_relay(endpoint.clone(), &target, &keypair, None).await {
                    Ok(t) => return t,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(300)).await,
                }
            }
        })
        .await;
        let (conn, send, recv, session_token) = match connected {
            Ok(t) => t,
            Err(_) => {
                crate::server_manager::stop_server(&procs, port)?;
                return Err("the relayed server did not register with the relay within 30 seconds".to_string());
            }
        };
        let link = crate::connection::build_relay_link(&target, "");
        (conn, send, recv, session_token, link, endpoint)
    } else {
        let address = format!("127.0.0.1:{}", port);
        // Wait for the direct server to be ready (poll up to 5 seconds).
        let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(endpoint) = crate::tls::make_client_endpoint() {
                    if let Ok(addr) = address.parse::<std::net::SocketAddr>() {
                        if let Ok(connecting) = endpoint.connect(addr, "farder-server") {
                            if let Ok(conn) = connecting.await {
                                conn.close(0u32.into(), b"probe");
                                return;
                            }
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .await;
        if ready.is_err() {
            crate::server_manager::stop_server(&procs, port)?;
            return Err("server failed to start within 5 seconds".to_string());
        }
        let endpoint = make_client_endpoint().map_err(|e| e.to_string())?;
        let addr: std::net::SocketAddr = address.parse().map_err(|e: std::net::AddrParseError| e.to_string())?;
        let (conn, send, recv, session_token) =
            connect_and_authenticate(endpoint.clone(), addr, &keypair, None, None)
                .await
                .map_err(|e| e.to_string())?;
        (conn, send, recv, session_token, address, endpoint)
    };
```

Then update the **shared tail** that follows: the datagram-recv loop is unchanged; the `ServerConnection` sets `relayed` (not the hard-coded `false`); `server_name` uses `name.clone()`; the avatar block, the `state.servers.insert`, `save_server_entry_with_config(&address, …)`, the `spawn_event_reader(app.clone(), address.clone(), …)`, and the final `GetServerInfo` all already use the `address` variable — they now transparently use the relay link for relayed servers. In the returned JSON, set `"relayed": relayed` (not `false`).

Concretely: in the `ServerConnection { … }` literal change `relayed: false,` to `relayed,`; and in the success JSON change `"relayed": false,` to `"relayed": relayed,`.

- [ ] **Step 6: Build the crate**

Run: `cd client/src-tauri && cargo build 2>&1 | tail -20`
Expected: compiles. Fix any borrow/type issues (e.g. `endpoint` is now bound in both branches and moved into `ServerConnection`). No `unused` warnings from the new code beyond pre-existing ones.

- [ ] **Step 7: Run the resolver tests + confirm nothing regressed**

Run: `cd client/src-tauri && cargo test relay_choice 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add client/src-tauri/src/commands.rs
git commit -m "Client: create_local_server can create a relayed server (Phase 4 piece 1)"
```

---

## Task 5: Relayed servers respawn in relay mode on relaunch

**Files:** Modify `client/src-tauri/src/commands.rs` (`restart_local_servers`).

- [ ] **Step 1: Update the relaunch logic**

In `restart_local_servers`, the per-entry respawn currently always calls `spawn_server_with_data_dir(name, template, data_dir)` and assigns a fresh `127.0.0.1:{port}` id. Make it relay-aware: if the entry's `id` is a relay link, respawn in relay mode and **keep the (stable) relay-link id**. Replace the body of the `if let Some(ref local) = entry.local { … }` branch with:

```rust
            reap_orphan_servers_for_data_dir(&local.data_dir);

            // A relayed server's id is its relay link (stable across restarts);
            // respawn it in relay mode and keep the same id. A direct server gets
            // a fresh local port (and id) each launch.
            let relay_addr = crate::connection::parse_relay_target(&entry.id).map(|t| t.relay_addr);
            match crate::server_manager::spawn_server_with_data_dir(
                &entry.name,
                &local.template,
                &local.data_dir,
                relay_addr,
            ) {
                Ok((info, child)) => {
                    let new_id = if relay_addr.is_some() {
                        entry.id.clone() // relay link is stable
                    } else {
                        format!("127.0.0.1:{}", info.port)
                    };
                    eprintln!("[restart] Respawned '{}' as {}", entry.name, new_id);
                    procs.register(info, child);
                    restarted.push(ServerEntry {
                        id: new_id,
                        name: entry.name.clone(),
                        local: entry.local.clone(),
                    });
                }
                Err(e) => {
                    eprintln!("Failed to restart local server '{}': {}", entry.name, e);
                }
            }
```

- [ ] **Step 2: Build**

Run: `cd client/src-tauri && cargo build 2>&1 | tail -15`
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add client/src-tauri/src/commands.rs
git commit -m "Client: respawn relayed local servers in relay mode on relaunch (Phase 4 piece 1)"
```

---

## Task 6: Create-server relay-choice UX

**Files:** Modify `client/src/lib/tauri-bridge.ts` and `client/src/components/AddServerModal.tsx`. Read both first.

- [ ] **Step 1: Extend the bridge call**

In `client/src/lib/tauri-bridge.ts`, update `createLocalServer` to accept and forward the relay choice. Add params `relayMode: "farder" | "selfhost" | "direct"`, `relayAddr?: string`, `relayFp?: string`, and pass them in the `invoke("create_local_server", { … })` object as `relayMode`, `relayAddr`, `relayFp` (matching the Rust command's `relay_mode`/`relay_addr`/`relay_fp` — Tauri converts camelCase JS keys to snake_case Rust args, consistent with the existing `iconPath` → `icon_path` convention in this file; follow whatever convention the file already uses).

- [ ] **Step 2: Add the reachability step to the modal**

In `client/src/components/AddServerModal.tsx`, add a reachability choice to the final create step (the template/privacy step, before "Create Server"). Add component state:

```tsx
  const [relayMode, setRelayMode] = useState<"farder" | "selfhost" | "direct">("farder");
  const [relayAddr, setRelayAddr] = useState("");
  const [relayFp, setRelayFp] = useState("");
  const [showRelayInfo, setShowRelayInfo] = useState(false);
```

Add this block to that step's JSX (above the Create button):

```tsx
  <div className="reachability">
    <label className="section-label">How will people reach your server?</label>

    <label className="relay-option">
      <input type="radio" name="relayMode" checked={relayMode === "farder"} onChange={() => setRelayMode("farder")} />
      <span><strong>Use the Farder relay</strong> <em>Recommended</em><br />
      <small>Members' IPs and yours stay hidden, and it works even behind a home router.</small></span>
    </label>

    <label className="relay-option">
      <input type="radio" name="relayMode" checked={relayMode === "selfhost"} onChange={() => setRelayMode("selfhost")} />
      <span><strong>Self-host your own relay</strong> <em>Advanced</em><br />
      <small>Point at a relay you run yourself.</small></span>
    </label>
    {relayMode === "selfhost" && (
      <div className="relay-selfhost-fields">
        <input placeholder="Relay address (host:port)" value={relayAddr} onChange={(e) => setRelayAddr(e.target.value)} />
        <input placeholder="Cert fingerprint (64 hex characters)" value={relayFp} onChange={(e) => setRelayFp(e.target.value)} />
      </div>
    )}

    <label className="relay-option">
      <input type="radio" name="relayMode" checked={relayMode === "direct"} onChange={() => setRelayMode("direct")} />
      <span><strong>Direct &mdash; same network only</strong> <em>Advanced</em><br />
      <small>Connects straight to your machine. Only reachable on your own network or with port-forwarding, and your IP is visible.</small></span>
    </label>

    <button type="button" className="learn-more-toggle" onClick={() => setShowRelayInfo(!showRelayInfo)}>
      {showRelayInfo ? "Hide details" : "Learn more"}
    </button>
    {showRelayInfo && (
      <div className="learn-more-body">
        <p>A relay is a neutral middle server. Because you and your members connect <em>through</em> it instead of directly to each other, neither side learns the other's IP address &mdash; and your server stays reachable even behind a home router.</p>
        <p>For this to protect you, the relay must be run by a neutral party (a relay run by the server's own host can't hide IPs from that host). The Farder relay is that neutral party.</p>
        <p><strong>One honest caveat:</strong> today a relay's operator can technically read a community's messages (they aren't yet end-to-end encrypted between members and the server). Your direct messages and voice are always end-to-end encrypted regardless. Removing even that is on the roadmap.</p>
      </div>
    )}
  </div>
```

- [ ] **Step 3: Block Create on incomplete self-host, and pass the choice**

In the modal's `handleCreate` (the function that calls `api.createLocalServer`), before the call add a guard:

```tsx
    if (relayMode === "selfhost" && (!relayAddr.trim() || !relayFp.trim())) {
      // surface an inline error / toast: both relay fields are required
      return;
    }
```

and pass the choice through the call:

```tsx
    const result = await api.createLocalServer(
      trimmedName,
      selectedTemplate,
      privacy,
      serverIcon ?? undefined,
      relayMode,
      relayMode === "selfhost" ? relayAddr.trim() : undefined,
      relayMode === "selfhost" ? relayFp.trim() : undefined,
    );
```

(Match the exact existing argument order/shape of `api.createLocalServer`; the relay args are appended after `serverIcon`.)

- [ ] **Step 4: Type-check**

Run: `cd client && npx tsc --noEmit`
Expected: passes with no errors. Resolve any type mismatches (e.g. the `createLocalServer` signature in `tauri-bridge.ts` must match the call).

- [ ] **Step 5: Commit**

```bash
git add client/src/lib/tauri-bridge.ts client/src/components/AddServerModal.tsx
git commit -m "Client UI: relay-choice step for creating a server (Phase 4 piece 1)"
```

---

## Task 7: Docs + final gates

**Files:** Modify `docs/modules/client-relay.md`.

- [ ] **Step 1: Final gates**

Run both and confirm green:
- `cd /home/deez/farder/client/src-tauri && cargo build` — client crate compiles.
- `cd /home/deez/farder/client && npx tsc --noEmit` — frontend types pass.
- `cd /home/deez/farder/client/src-tauri && cargo test default_relay server_manager relay_choice relay_link 2>&1 | tail -20` — the unit tests pass.
- Seam check: `grep -n "create_local_server" client/src-tauri/src/main.rs` — still in `generate_handler!` (no new command).

- [ ] **Step 2: Update `docs/modules/client-relay.md`**

Add a "Creating a relayed server (Phase 4 piece 1)" section: the create-server flow now offers Farder-relay / self-host / direct; a relayed server is spawned with `--relay`/`--data-dir` (client-generated `server_id` pre-written), the creator connects via `connect_via_relay` with an empty owner token (auto-claims owner), and the **relay link** is saved as the server entry id (stable across relaunch; respawned in relay mode). Note `default_relay()` is the accessor for the configured relay. Flag that the end-to-end create→register→join flow is verified on a Windows run against the live relay (`45.77.70.199:4433`) and is UNVERIFIED until then.

- [ ] **Step 3: Commit**

```bash
git add docs/modules/client-relay.md
git commit -m "Docs: in-app relayed-server creation (Phase 4 piece 1)"
```

---

## Final verification

- [ ] `cd client/src-tauri && cargo build` — green.
- [ ] `cd client && npx tsc --noEmit` — green.
- [ ] Unit tests pass: `default_relay` (accessor), `server_manager` (arg-builder), `relay_choice` (resolver/validation), `relay_link` (empty-token round-trip).
- [ ] Seam intact (`create_local_server` still registered; no new command).
- [ ] Spec coverage: accessor (T1); relay spawn (T2); empty owner token (T3); create branch + self-host validation (T4); relaunch respawn (T5); UX + learn-more (T6); docs (T7).
- [ ] **UNVERIFIED, by design — but now runnable:** the full create-relayed-server → registers with the live relay → owner claimed → invite a 2nd client → join → text + voice. The user's Windows run against `45.77.70.199:4433`. State this plainly.

After all tasks: use **superpowers:finishing-a-development-branch** to complete the work.
