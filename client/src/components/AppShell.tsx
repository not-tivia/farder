import { useEffect } from "react";
import TitleBar from "./TitleBar";
import ChannelSidebar from "./ChannelSidebar";
import ChatPanel from "./ChatPanel";
import MemberSidebar from "./MemberSidebar";
import DmPanel from "./DmPanel";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

export default function AppShell() {
  const { state, dispatch } = useServer();

  // Subscribe to all active channels whenever they change
  useEffect(() => {
    if (!state.connected) return;
    const ids: number[] = [];
    if (state.currentChannelId) ids.push(state.currentChannelId);
    if (state.dmPanelChannelId && state.dmPanelChannelId !== state.currentChannelId) {
      ids.push(state.dmPanelChannelId);
    }
    if (state.threadChannelId && !ids.includes(state.threadChannelId)) {
      ids.push(state.threadChannelId);
    }
    if (ids.length > 0) {
      api.subscribeChannels(ids).catch(() => {});
    }
  }, [state.connected, state.currentChannelId, state.dmPanelChannelId, state.threadChannelId]);

  useEffect(() => {
    if (!state.connectionLost) return;
    let cancelled = false;
    async function tryReconnect() {
      while (!cancelled) {
        try {
          await api.loadIdentity();
          const server = await api.getLastServer();
          if (!server) break;
          const result = await api.connectServer(server);
          dispatch({ type: "RECONNECTED" });
          dispatch({ type: "CONNECTED", payload: result });
          // Re-fetch members
          const members = await api.getMembers();
          dispatch({ type: "SET_MEMBERS", payload: members });
          // Re-load DMs
          try {
            const dms = await api.listDms();
            dispatch({ type: "SET_DMS", payload: dms });
          } catch {}
          // Re-subscribe to current channel
          if (state.currentChannelId) {
            await api.subscribeChannels([state.currentChannelId]);
          }
          break;
        } catch {
          await new Promise((r) => setTimeout(r, 3000));
        }
      }
    }
    tryReconnect();
    return () => {
      cancelled = true;
    };
  }, [state.connectionLost]);

  return (
    <>
      <TitleBar />
      <div className="main-layout" style={{ position: "relative" }}>
        <ChannelSidebar />
        <ChatPanel />
        <MemberSidebar />
        <DmPanel />
        {state.connectionLost && (
          <div className="reconnect-overlay">
            Connection lost. Reconnecting...
          </div>
        )}
      </div>
    </>
  );
}
