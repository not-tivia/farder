import { useEffect } from "react";
import TitleBar from "./TitleBar";
import ChannelSidebar from "./ChannelSidebar";
import ChatPanel from "./ChatPanel";
import MemberSidebar from "./MemberSidebar";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

export default function AppShell() {
  const { state, dispatch } = useServer();

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
        {state.connectionLost && (
          <div className="reconnect-overlay">
            Connection lost. Reconnecting...
          </div>
        )}
      </div>
    </>
  );
}
