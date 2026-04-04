import { useState, useEffect } from "react";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import type { ChannelInfo, CategoryInfo } from "../lib/types";

function UserFooter() {
  const [key, setKey] = useState<string | null>(null);

  useEffect(() => {
    api.getPublicKey().then((k) => setKey(k)).catch(() => {});
  }, []);

  return <span>{key ?? "No identity"}</span>;
}

export default function ChannelSidebar() {
  const { state, dispatch } = useServer();

  async function handleSelectChannel(channel: ChannelInfo) {
    dispatch({ type: "SELECT_CHANNEL", payload: channel.id });
    try {
      await api.subscribeChannels([channel.id]);
      const msgs = await api.fetchHistory(channel.id);
      dispatch({ type: "SET_MESSAGES", payload: { channelId: channel.id, messages: msgs } });
    } catch {
      // ignore fetch errors
    }
  }

  // Exclude Thread channels from the sidebar (they appear inline)
  const visibleChannels = state.channels.filter((c) => c.channel_type !== "Thread");
  const sortedCategories = [...state.categories].sort((a, b) => a.position - b.position);
  const uncategorized = visibleChannels
    .filter((c) => c.category_id === null)
    .sort((a, b) => a.position - b.position);

  function renderChannel(ch: ChannelInfo) {
    const isActive = ch.id === state.currentChannelId;
    return (
      <div
        key={ch.id}
        className={`channel-item${isActive ? " active" : ""}`}
        onClick={() => handleSelectChannel(ch)}
      >
        <span className="channel-prefix">#</span>
        <span>{ch.name}</span>
      </div>
    );
  }

  function renderCategory(cat: CategoryInfo) {
    const catChannels = visibleChannels
      .filter((c) => c.category_id === cat.id)
      .sort((a, b) => a.position - b.position);
    if (catChannels.length === 0) return null;
    return (
      <div key={cat.id}>
        <div className="channel-category">{cat.name}</div>
        {catChannels.map(renderChannel)}
      </div>
    );
  }

  return (
    <div className="channel-sidebar">
      <div className="server-header">
        <div className="server-name">{state.serverName}</div>
      </div>
      <div className="channel-list">
        {uncategorized.map(renderChannel)}
        {sortedCategories.map(renderCategory)}
      </div>
      <div className="user-footer">
        <UserFooter />
      </div>
    </div>
  );
}
