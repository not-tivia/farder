import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { AppProvider, useApp } from "./context/ServerContext";
import { useServerEvents } from "./hooks/useServerEvents";
import ConnectDialog from "./components/ConnectDialog";
import AppShell from "./components/AppShell";
import * as api from "./lib/tauri-bridge";

function AppInner() {
  const { state, dispatch } = useApp();
  useServerEvents();

  // Handle farder:// deep links passed via CLI argument at launch.
  // URL format: farder://<host:port>/<invite_code>
  useEffect(() => {
    const unlisten = listen<string>("deep-link", (e) => {
      const url = e.payload;
      const match = url.match(/^farder:\/\/([^/]+)\/(.+)$/);
      if (!match) return;
      const address = match[1];
      const inviteCode = match[2];
      // Pre-fill the connect dialog with the invite details by dispatching a
      // pending invite action that ConnectDialog can read from state.
      dispatch({ type: "SET_PENDING_INVITE", payload: { address, inviteCode } });
    });
    return () => { unlisten.then((u) => u()); };
  }, [dispatch]);

  useEffect(() => {
    async function init() {
      const key = await api.loadIdentity();
      if (!key) return; // show onboarding (ConnectDialog handles identity setup)
      dispatch({ type: "SET_IDENTITY" });

      const savedServers = await api.getSavedServers();
      if (savedServers.length === 0) return; // show first-server dialog

      // Connect to all saved servers
      for (const server of savedServers) {
        try {
          const result = await api.connectServer(server.id);
          dispatch({ type: "SERVER_ADDED", serverId: server.id, payload: result });
        } catch {
          // Skip failed servers
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
    }
    init().catch(() => {});
  }, []);

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
