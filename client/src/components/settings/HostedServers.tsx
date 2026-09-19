import { useCallback, useEffect, useState } from "react";
import * as api from "../../lib/tauri-bridge";
import SettingsSection from "./SettingsSection";

// ---------------------------------------------------------------------------
// The servers THIS machine is hosting.
//
// Creating a server from the Add Server dialog spawns a `farder-server` process
// that this client supervises. Until now nothing showed those processes and
// nothing could stop one: `get_local_servers` and `stop_local_server` were both
// registered with no caller. They are killed when the app exits, so nothing
// leaked, but a server you started by mistake ran until you quit the whole app.
// ---------------------------------------------------------------------------

export default function HostedServers() {
  const [servers, setServers] = useState<api.ManagedServer[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [stopping, setStopping] = useState<number | null>(null);

  const refresh = useCallback(async () => {
    try {
      setServers(await api.getLocalServers());
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);

  async function handleStop(s: api.ManagedServer) {
    // Stopping a server takes it offline for everyone connected to it, which is
    // not something to do on a single click.
    const ok = window.confirm(
      `Stop "${s.name}"?\n\n` +
      "Members lose their connection until you start it again, and it stays " +
      "stopped until then. Nothing is deleted -- its data stays on this machine.",
    );
    if (!ok) return;
    setStopping(s.port);
    try {
      await api.stopLocalServer(s.port);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setStopping(null);
    }
  }

  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">Hosted Servers</h2>

      <SettingsSection label="Running On This Machine">
        {error && <div className="error-text">{error}</div>}
        {servers === null && !error && <p className="settings-help">Loading...</p>}
        {servers !== null && servers.length === 0 && (
          <p className="settings-help">
            This machine is not hosting any servers right now. Creating one from
            "Add a Server" starts it here.
          </p>
        )}
        {(servers ?? []).map((s) => (
          <div key={s.port} className="organizer-row">
            <span className="organizer-name">
              {s.name}
              <span style={{ color: "var(--xp-text-muted)", fontSize: 11 }}>
                {" "}
                {s.relayed ? "via the relay" : `port ${s.port}`} - {s.privacy}
              </span>
            </span>
            <button
              className="xp-button"
              disabled={stopping === s.port}
              onClick={() => { void handleStop(s); }}
            >
              {stopping === s.port ? "Stopping..." : "Stop"}
            </button>
          </div>
        ))}
        <p className="settings-help">
          A hosted server runs as a separate process supervised by this app, and all
          of them stop when you quit. Stopping one here does not delete anything.
        </p>
      </SettingsSection>
    </div>
  );
}
