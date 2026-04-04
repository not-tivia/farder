import { useState, useEffect } from "react";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import type { ChannelInfo, CategoryInfo } from "../lib/types";
import InviteDialog from "./InviteDialog";

function UserFooter() {
  const [name, setName] = useState<string | null>(null);

  useEffect(() => {
    api.getDisplayName().then((n) => setName(n)).catch(() => {});
  }, []);

  return <span>● {name ?? "Unknown"}</span>;
}

export default function ChannelSidebar() {
  const { state, dispatch } = useServer();
  const [showInvite, setShowInvite] = useState(false);

  async function handleSelectChannel(channel: ChannelInfo) {
    dispatch({ type: "SELECT_CHANNEL", payload: channel.id });
    try {
      await api.subscribeChannels([channel.id]);
      const msgs = await api.fetchHistory(channel.id);
      // Server returns newest-first; reverse for chronological display
      const reversed = msgs.reverse();
      dispatch({ type: "SET_MESSAGES", payload: { channelId: channel.id, messages: reversed } });
      // Mark channel as read with latest message id
      if (reversed.length > 0) {
        const latestId = Math.max(...reversed.map((m) => m.id));
        dispatch({ type: "MARK_READ", payload: { channelId: channel.id, lastMessageId: latestId } });
      }
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
    const lastRead = state.readState?.[ch.id] ?? 0;
    const channelMsgs = state.messages[ch.id] ?? [];
    const hasUnread = channelMsgs.some((m) => m.id > lastRead) && ch.id !== state.currentChannelId;
    return (
      <div
        key={ch.id}
        className={`channel-item${isActive ? " active" : ""}${hasUnread ? " unread" : ""}`}
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
    <>
      <div className="channel-sidebar">
        <div className="server-header">
          <div className="server-name">{state.serverName}</div>
          <button className="server-invite-btn" onClick={() => setShowInvite(true)} title="Create Invite">+</button>
        </div>
        <div className="channel-list">
          {uncategorized.map(renderChannel)}
          {sortedCategories.map(renderCategory)}
        </div>
        <div className="user-footer">
          <UserFooter />
        </div>
      </div>
      {showInvite && <InviteDialog onClose={() => setShowInvite(false)} />}
    </>
  );
}
