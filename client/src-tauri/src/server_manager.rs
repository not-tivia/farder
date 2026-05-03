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
    children: Mutex<HashMap<u16, (ManagedServer, CommandChild)>>,
}

impl ServerProcesses {
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, info: ManagedServer, child: CommandChild) {
        let port = info.port;
        self.children.lock().unwrap().insert(port, (info, child));
    }

    pub fn list(&self) -> Vec<ManagedServer> {
        self.children.lock().unwrap().values().map(|(info, _)| info.clone()).collect()
    }

    pub fn stop_all(&self) {
        let mut children = self.children.lock().unwrap();
        for (_port, (_info, child)) in children.drain() {
            let _ = child.kill();
        }
    }
}

/// Find an available TCP port starting from `start`.
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

/// Spawn a farder-server sidecar process with the given configuration.
pub fn spawn_server(
    app: &AppHandle,
    name: &str,
    template: &str,
    privacy: &str,
) -> Result<(ManagedServer, CommandChild), String> {
    let port = find_available_port(4435)
        .ok_or_else(|| "no available port found (tried 4435-4534)".to_string())?;

    let data_dir = server_data_dir(name)?;
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

    Ok((info, child))
}

/// Stop a locally-managed server by port.
pub fn stop_server(procs: &ServerProcesses, port: u16) -> Result<(), String> {
    let mut children = procs.children.lock().unwrap();
    if let Some((_info, child)) = children.remove(&port) {
        child.kill().map_err(|e| format!("failed to kill server: {}", e))?;
    }
    Ok(())
}
