import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { AppProvider, useApp } from "./context/ServerContext";
import { MediaPlayersProvider } from "./context/MediaPlayersContext";
import { DataSaverProvider } from "./context/DataSaverContext";
import { useServerEvents } from "./hooks/useServerEvents";
import { useMlsSteward } from "./hooks/useMlsSteward";
import ConnectDialog from "./components/ConnectDialog";
import AppShell from "./components/AppShell";
import IdentityGate from "./components/IdentityGate";
import { TranslationFirstRunModal } from "./components/TranslationFirstRunModal";
import ToastContainer from "./components/ToastContainer";
import JoinConfirmModal from "./components/JoinConfirmModal";
import * as api from "./lib/tauri-bridge";
import { parseInviteLink } from "./lib/invite";

// Module-level guard: React StrictMode in dev mounts the root twice, which
// would otherwise run init() twice — spawning duplicate local servers and
// opening two QUIC sessions per identity (and causing the server to tear
// itself down via a races on its `clients` map).
let initStarted = false;

function AppInner() {
  const { state, dispatch } = useApp();
  const [initializing, setInitializing] = useState(true);
  const [unlocked, setUnlocked] = useState(false);
  const [pendingInvite, setPendingInvite] = useState<string | null>(null);
  useServerEvents();
  useMlsSteward();

  // Handle farder:// deep links passed via CLI argument at launch.
  // The link can arrive before the identity is unlocked, so just queue it;
  // a separate effect processes it once `unlocked` becomes true.
  useEffect(() => {
    const unlisten = listen<string>("deep-link", (e) => {
      setPendingInvite(e.payload);
    });
    return () => { unlisten.then((u) => u()); };
  }, []);

  useEffect(() => {
    if (!unlocked) return;
    if (initStarted) return;
    initStarted = true;
    async function init() {
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

      // Activate the last-used server (fall back to first if missing/not found)
      if (savedServers.length > 0) {
        const lastServerId = await api.getLastServer().catch(() => null);
        const connectedIds = new Set(savedServers.map((s) => s.id));
        const targetId =
          (lastServerId && connectedIds.has(lastServerId) ? lastServerId : null) ??
          savedServers[0].id;
        dispatch({ type: "SET_ACTIVE_SERVER", serverId: targetId });
        try {
          const members = await api.getMembers(targetId);
          dispatch({ type: "SET_MEMBERS", serverId: targetId, payload: members });
          const dms = await api.listDms(targetId);
          dispatch({ type: "SET_DMS", serverId: targetId, payload: dms });
        } catch {}
      }

      setInitializing(false);
    }
    init().catch(() => setInitializing(false));
  }, [unlocked]);

  async function joinFromInvite(url: string) {
    const parsed = parseInviteLink(url);
    if (!parsed.address) {
      console.error("[deep-link] unrecognized invite:", url);
      return;
    }
    try {
      const result = await api.connectServer(parsed.address, parsed.inviteCode, parsed.setupToken);
      dispatch({ type: "SERVER_ADDED", serverId: parsed.address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: parsed.address });
      if (parsed.inviteCode && result.server_id) {
        try { await api.joinLogServer(parsed.address, result.server_id, parsed.inviteCode); }
        catch (e) { console.error("[mesh] join_log_server failed:", e); }
      }
      try { const members = await api.getMembers(parsed.address); dispatch({ type: "SET_MEMBERS", serverId: parsed.address, payload: members }); } catch {}
      try { const dms = await api.listDms(parsed.address); dispatch({ type: "SET_DMS", serverId: parsed.address, payload: dms }); } catch {}
    } catch (e) {
      console.error("[deep-link] failed to join from invite:", e);
    }
  }

  // Process a queued farder:// deep link once the identity is unlocked.
  // Open the confirmation modal instead of auto-joining.
  useEffect(() => {
    if (!unlocked || !pendingInvite) return;
    dispatch({ type: "OPEN_JOIN_CONFIRM", link: pendingInvite });
    setPendingInvite(null);
  }, [unlocked, pendingInvite]);

  if (!unlocked) {
    return <IdentityGate onUnlocked={() => setUnlocked(true)} />;
  }

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

  // Modal for confirming a join from an invite link. Extracted so every
  // post-unlock return branch can render it (a brand-new user with no servers
  // who clicks an invite must still see the confirm prompt).
  const confirmModal = state.joinConfirmLink ? (
    <JoinConfirmModal
      relayed={/^farder:\/\/relayd?\//i.test(parseInviteLink(state.joinConfirmLink).address ?? "")}
      link={state.joinConfirmLink}
      onConfirm={() => { const u = state.joinConfirmLink!; dispatch({ type: "CLOSE_JOIN_CONFIRM" }); void joinFromInvite(u); }}
      onCancel={() => dispatch({ type: "CLOSE_JOIN_CONFIRM" })}
    />
  ) : null;

  // Show onboarding if no identity yet
  if (!state.hasIdentity) {
    return (
      <>
        <ConnectDialog />
        {confirmModal}
      </>
    );
  }

  // Show first-server dialog if identity exists but no servers
  if (state.serverList.length === 0) {
    return (
      <>
        <ConnectDialog />
        {confirmModal}
      </>
    );
  }

  // Show main app shell
  return (
    <>
      <AppShell />
      <TranslationFirstRunModal />
      {confirmModal}
    </>
  );
}

export default function App() {
  return (
    <AppProvider>
      <MediaPlayersProvider>
        <DataSaverProvider>
          <AppInner />
        </DataSaverProvider>
      </MediaPlayersProvider>
      <ToastContainer />
    </AppProvider>
  );
}
