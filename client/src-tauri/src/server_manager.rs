use std::collections::HashMap;
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
        self.children.lock().unwrap().insert(port, (info, child));
    }

    pub fn list(&self) -> Vec<ManagedServer> {
        self.children.lock().unwrap().values().map(|(info, _)| info.clone()).collect()
    }

    pub fn stop_all(&self) {
        let mut children = self.children.lock().unwrap();
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

/// Spawn a farder-server process with the given configuration.
pub fn spawn_server(
    name: &str,
    template: &str,
    _privacy: &str,
) -> Result<(ManagedServer, Child), String> {
    let port = find_available_port(4435)
        .ok_or_else(|| "no available port found (tried 4435-4534)".to_string())?;

    let data_dir = server_data_dir(name)?;
    let db_path = data_dir.join("server.db");
    let files_path = data_dir.join("files");

    let bind_addr = format!("0.0.0.0:{}", port);
    let server_bin = find_server_binary()?;

    let child = Command::new(&server_bin)
        .args([
            "--bind", &bind_addr,
            "--name", name,
            "--template", template,
            "--db", &db_path.to_string_lossy(),
            "--storage-dir", &files_path.to_string_lossy(),
        ])
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
    };

    Ok((info, child))
}

/// Spawn a server using an existing data directory (for restarting on app relaunch).
pub fn spawn_server_with_data_dir(
    name: &str,
    template: &str,
    data_dir: &str,
) -> Result<(ManagedServer, Child), String> {
    let port = find_available_port(4435)
        .ok_or_else(|| "no available port found (tried 4435-4534)".to_string())?;

    let data_path = PathBuf::from(data_dir);
    let db_path = data_path.join("server.db");
    let files_path = data_path.join("files");
    let bind_addr = format!("0.0.0.0:{}", port);
    let server_bin = find_server_binary()?;

    let child = Command::new(&server_bin)
        .args([
            "--bind", &bind_addr,
            "--name", name,
            "--template", template,
            "--db", &db_path.to_string_lossy(),
            "--storage-dir", &files_path.to_string_lossy(),
        ])
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
    };

    Ok((info, child))
}

/// Stop a locally-managed server by port.
pub fn stop_server(procs: &ServerProcesses, port: u16) -> Result<(), String> {
    let mut children = procs.children.lock().unwrap();
    if let Some((_info, ref mut child)) = children.remove(&port) {
        child.kill().map_err(|e| format!("failed to kill server: {}", e))?;
        let _ = child.wait();
    }
    Ok(())
}
