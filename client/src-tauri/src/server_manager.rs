use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

/// Information about a managed (locally-spawned) server.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedServer {
    pub name: String,
    pub port: u16,
    pub data_dir: String,
    pub template: String,
    pub privacy: String, // "invite-only" or "open"
    #[serde(default)]
    pub relayed: bool,
}

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

/// Tracks all locally-spawned server processes.
pub struct ServerProcesses {
    children: Mutex<HashMap<u16, (ManagedServer, Child)>>,
}

impl ServerProcesses {
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, info: ManagedServer, child: Child) {
        let port = info.port;
        self.children.lock().unwrap_or_else(|e| e.into_inner()).insert(port, (info, child));
    }

    pub fn list(&self) -> Vec<ManagedServer> {
        self.children.lock().unwrap_or_else(|e| e.into_inner()).values().map(|(info, _)| info.clone()).collect()
    }

    pub fn stop_all(&self) {
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        for (_port, (_info, ref mut child)) in children.drain() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Find an available UDP port starting from `start`.
fn find_available_port(start: u16) -> Option<u16> {
    (start..start + 100).find(|&port| {
        std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok()
    })
}

/// Resolve the data directory for a server. Creates it if needed.
fn server_data_dir(server_name: &str) -> Result<PathBuf, String> {
    let safe_name = server_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>();
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("farder")
        .join("servers")
        .join(&safe_name);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create server data dir {:?}: {}", dir, e))?;
    let files_dir = dir.join("files");
    std::fs::create_dir_all(&files_dir)
        .map_err(|e| format!("failed to create files dir {:?}: {}", files_dir, e))?;
    Ok(dir)
}

/// Find the farder-server binary. Checks:
/// 1. Next to the current executable (sidecar location for production builds)
/// 2. The workspace target/debug directory (for dev builds)
/// 3. System PATH
fn find_server_binary() -> Result<PathBuf, String> {
    // Check next to current executable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let candidate = exe_dir.join("farder-server");
            if candidate.exists() {
                return Ok(candidate);
            }
            // Also check with target triple suffix
            let triple = env!("TARGET");
            let candidate = exe_dir.join(format!("farder-server-{}", triple));
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // Check if farder-server is on PATH (try running it directly — works cross-platform)
    if let Ok(output) = Command::new("farder-server").arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status() {
        if output.success() {
            return Ok(PathBuf::from("farder-server"));
        }
    }

    Err("could not find farder-server binary — build it with 'cargo build -p farder-server'".to_string())
}

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

/// Stop a locally-managed server by port.
pub fn stop_server(procs: &ServerProcesses, port: u16) -> Result<(), String> {
    let mut children = procs.children.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_info, ref mut child)) = children.remove(&port) {
        child.kill().map_err(|e| format!("failed to kill server: {}", e))?;
        let _ = child.wait();
    }
    Ok(())
}
