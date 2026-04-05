import { useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import AddServerModal from "./AddServerModal";

export default function ServerStrip() {
  const { state, dispatch } = useApp();
  const [showAdd, setShowAdd] = useState(false);

  async function handleSelectServer(serverId: string) {
    dispatch({ type: "SET_ACTIVE_SERVER", serverId });
    dispatch({ type: "CLEAR_UNREAD", serverId });
    try {
      const info = await api.getServerInfo(serverId);
      dispatch({ type: "SERVER_REFRESHED", serverId, payload: info });
      const members = await api.getMembers(serverId);
      dispatch({ type: "SET_MEMBERS", serverId, payload: members });
      const dms = await api.listDms(serverId);
      dispatch({ type: "SET_DMS", serverId, payload: dms });
    } catch {}
  }

  return (
    <div className="server-strip">
      {state.serverList.map((s) => {
        const isActive = s.id === state.activeServerId;
        return (
          <div
            key={s.id}
            className={`server-icon${isActive ? " active" : ""}`}
            onClick={() => handleSelectServer(s.id)}
            title={s.name}
          >
            {s.name.charAt(0).toUpperCase()}
            {s.unreadCount > 0 && !isActive && <span className="server-unread-dot" />}
            {s.hasMention && !isActive && <span className="server-mention-badge" />}
          </div>
        );
      })}
      <div className="server-strip-separator" />
      <div className="server-icon add-server" onClick={() => setShowAdd(true)} title="Add Server">+</div>
      {showAdd && <AddServerModal onClose={() => setShowAdd(false)} />}
    </div>
  );
}
