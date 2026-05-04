import { useEffect } from "react";
import TitleBar from "./TitleBar";
import ChannelSidebar from "./ChannelSidebar";
import ChatPanel from "./ChatPanel";
import MemberSidebar from "./MemberSidebar";
import DmPanel from "./DmPanel";
import ServerStrip from "./ServerStrip";
import { useApp, useActiveServer, useActiveServerId } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

export default function AppShell() {
  const { dispatch } = useApp();
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();

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
      api.subscribeChannels(serverId, ids).catch(() => {});
    }
  }, [serverId, activeServer?.connected, activeServer?.currentChannelId, activeServer?.dmPanelChannelId, activeServer?.threadChannelId]);

  // Reconnect logic per-server
  useEffect(() => {
    if (!serverId || !activeServer?.connectionLost) return;
    let cancelled = false;
    async function tryReconnect() {
      while (!cancelled && serverId) {
        try {
          await api.loadIdentity();
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

  return (
    <>
      <TitleBar />
      <div className="main-layout" style={{ position: "relative" }}>
        <ServerStrip />
        <ChannelSidebar />
        <ChatPanel />
        <MemberSidebar />
        <DmPanel />
        {activeServer?.connectionLost && (
          <div className="reconnect-overlay">
            Connection lost. Reconnecting...
          </div>
        )}
      </div>
    </>
  );
}
