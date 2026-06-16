import { useEffect, useState } from "react";
import TitleBar from "./TitleBar";
import ChannelSidebar from "./ChannelSidebar";
import ChatPanel from "./ChatPanel";
import ScreenShareStage from "./ScreenShareStage";
import { MessageSearchOverlay } from "./MessageSearchOverlay";
import MemberSidebar from "./MemberSidebar";
import DmPanel from "./DmPanel";
import ServerStrip from "./ServerStrip";
import KickedBannedDialog from "./KickedBannedDialog";
import { useApp, useActiveServer, useActiveServerId } from "../context/ServerContext";
import { useVoice } from "../hooks/useVoice";
import * as api from "../lib/tauri-bridge";

let openSearchRef: (() => void) | null = null;

/** Called from outside AppShell (e.g., ChatPanel's search button) to open the
 *  message-search overlay. No-op until AppShell mounts. */
export function openMessageSearch(): void {
  openSearchRef?.();
}

export default function AppShell() {
  const { state, dispatch } = useApp();
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();

  // Single voice instance for the whole shell, threaded to the components that
  // need it. Sharing one instance keeps `watching`/`isSharing` consistent
  // between the sidebar's Join button and the main-area screen-share stage, and
  // avoids duplicate voice event listeners (which would double-decode video).
  const voice = useVoice();

  const [searchOpen, setSearchOpen] = useState(false);
  // Counter that increments every time something tries to open the overlay.
  // The overlay treats it as a "refocus me" trigger so Ctrl+K refocuses the
  // input even when the overlay is already mounted (which setSearchOpen(true)
  // alone wouldn't trigger because React dedups identical state updates).
  const [openTrigger, setOpenTrigger] = useState(0);

  // Register the module-level ref so external callers (e.g. ChatPanel) can open
  // the overlay via openMessageSearch().
  useEffect(() => {
    openSearchRef = () => {
      setSearchOpen(true);
      setOpenTrigger((n) => n + 1);
    };
    return () => { openSearchRef = null; };
  }, []);

  // Global Ctrl+K (Cmd+K on macOS) opens / refocuses the search overlay.
  useEffect(() => {
    const isMac = navigator.platform.toUpperCase().startsWith("MAC");
    const handler = (e: KeyboardEvent) => {
      const modPressed = isMac ? e.metaKey : e.ctrlKey;
      if (modPressed && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSearchOpen(true);
        setOpenTrigger((n) => n + 1);
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, []);

  // Subscribe to all active channels whenever they change
  useEffect(() => {
    if (!serverId || !activeServer?.connected) return;
    const ids: number[] = [];
    if (activeServer.currentChannelId) ids.push(activeServer.currentChannelId);
    if (activeServer.dmPanelChannelId && activeServer.dmPanelChannelId !== activeServer.currentChannelId) {
      ids.push(activeServer.dmPanelChannelId);
    }
    if (activeServer.threadChannelId && !ids.includes(activeServer.threadChannelId)) {
      ids.push(activeServer.threadChannelId);
    }
    if (ids.length > 0) {
      api.subscribeChannels(serverId, ids).catch((e) => console.error("[subscribe] failed; channel may stop receiving messages:", e));
    }
  }, [serverId, activeServer?.connected, activeServer?.currentChannelId, activeServer?.dmPanelChannelId, activeServer?.threadChannelId]);

  // Reconnect logic per-server
  useEffect(() => {
    if (!serverId || !activeServer?.connectionLost) return;
    let cancelled = false;
    async function tryReconnect() {
      while (!cancelled && serverId) {
        try {
          await api.getPublicKey();
          const result = await api.connectServer(serverId);
          dispatch({ type: "RECONNECTED", serverId });
          dispatch({ type: "CONNECTED", serverId, payload: result });
          const members = await api.getMembers(serverId);
          dispatch({ type: "SET_MEMBERS", serverId, payload: members });
          try {
            const dms = await api.listDms(serverId);
            dispatch({ type: "SET_DMS", serverId, payload: dms });
          } catch {}
          if (activeServer?.currentChannelId) {
            await api.subscribeChannels(serverId, [activeServer.currentChannelId]);
          }
          break;
        } catch (e) {
          console.error(`[reconnect] ${serverId}:`, e);
          await new Promise((r) => setTimeout(r, 3000));
        }
      }
    }
    tryReconnect();
    return () => {
      cancelled = true;
    };
  }, [serverId, activeServer?.connectionLost]);

  // On connection loss, tear down any active voice call so the mic stops and we
  // don't show a zombie "in call" state over a dead connection.
  useEffect(() => {
    if (serverId && activeServer?.connectionLost && activeServer?.currentVoiceChannelId != null) {
      api.voiceLeave().catch(() => {});
      dispatch({ type: "LEAVE_VOICE_CHANNEL", serverId });
    }
  }, [serverId, activeServer?.connectionLost, activeServer?.currentVoiceChannelId]);

  return (
    <>
      <TitleBar />
      <div className="main-layout" style={{ position: "relative" }}>
        <ServerStrip />
        <ChannelSidebar voice={voice} />
        <ChatPanel />
        <MemberSidebar />
        <ScreenShareStage voice={voice} />
        <DmPanel />
        {activeServer?.connectionLost && (
          <div className="reconnect-overlay">
            Connection lost. Reconnecting...
          </div>
        )}
        {state.kickedBanned && (
          <KickedBannedDialog
            kind={state.kickedBanned.kind}
            serverName={
              state.serverList.find((s) => s.id === state.kickedBanned!.serverId)?.name ?? "the server"
            }
            reason={state.kickedBanned.reason}
            onClose={() => {
              const sid = state.kickedBanned!.serverId;
              dispatch({ type: "CLEAR_KICKED_BANNED" });
              // Stop any voice call (mic) and clear the in-call UI for this server.
              api.voiceLeave().catch(() => {});
              dispatch({ type: "LEAVE_VOICE_CHANNEL", serverId: sid });
              // Also disconnect the server so the dialog doesn't recur on reconnect.
              api.disconnectServer(sid).catch(() => {});
            }}
          />
        )}
        <MessageSearchOverlay open={searchOpen} openTrigger={openTrigger} onClose={() => setSearchOpen(false)} />
      </div>
    </>
  );
}
