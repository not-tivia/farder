import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { AppProvider, useApp } from "./context/ServerContext";
import { useServerEvents } from "./hooks/useServerEvents";
import ConnectDialog from "./components/ConnectDialog";
import AppShell from "./components/AppShell";
import * as api from "./lib/tauri-bridge";

function AppInner() {
  const { state, dispatch } = useApp();
  const [initializing, setInitializing] = useState(true);
  useServerEvents();

  // Handle farder:// deep links passed via CLI argument at launch.
  useEffect(() => {
    const unlisten = listen<string>("deep-link", (e) => {
      const url = e.payload;
      const match = url.match(/^farder:\/\/([^/]+)\/(.+)$/);
      if (!match) return;
      const address = match[1];
      const inviteCode = match[2];
      console.log("[deep-link] invite:", address, inviteCode);
    });
    return () => { unlisten.then((u) => u()); };
  }, [dispatch]);

  useEffect(() => {
    async function init() {
      const key = await api.loadIdentity();
      if (!key) { setInitializing(false); return; }
      dispatch({ type: "SET_IDENTITY" });

      // Restart any locally-managed servers first, then get the updated list
      let savedServers: { id: string; name: string }[];
      try {
        savedServers = await api.restartLocalServers();
      } catch {
        savedServers = await api.getSavedServers();
      }
      if (savedServers.length === 0) { setInitializing(false); return; }

      // Wait for local servers to become ready
      await new Promise(r => setTimeout(r, 2000));

      // Connect to all saved servers
      for (const server of savedServers) {
        try {
          const result = await api.connectServer(server.id);
          dispatch({ type: "SERVER_ADDED", serverId: server.id, payload: result });
        } catch (e) {
          console.error(`[init] failed to connect to ${server.name} (${server.id}):`, e);
        }
      }

      // Activate the first one
      if (savedServers.length > 0) {
        const firstId = savedServers[0].id;
        dispatch({ type: "SET_ACTIVE_SERVER", serverId: firstId });
        try {
          const members = await api.getMembers(firstId);
          dispatch({ type: "SET_MEMBERS", serverId: firstId, payload: members });
          const dms = await api.listDms(firstId);
          dispatch({ type: "SET_DMS", serverId: firstId, payload: dms });
        } catch {}
      }

      setInitializing(false);
    }
    init().catch(() => setInitializing(false));
  }, []);

  // Still loading — show nothing (or a splash screen later)
  if (initializing) {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Farder</div>
          <div className="connect-dialog-body" style={{ textAlign: "center", padding: 32 }}>
            Connecting...
          </div>
        </div>
      </div>
    );
  }

  // Show onboarding if no identity yet
  if (!state.hasIdentity) {
    return <ConnectDialog />;
  }

  // Show first-server dialog if identity exists but no servers
  if (state.serverList.length === 0) {
    return <ConnectDialog />;
  }

  // Show main app shell
  return <AppShell />;
}

export default function App() {
  return (
    <AppProvider>
      <AppInner />
    </AppProvider>
  );
}
