import { useEffect, useRef } from "react";
import { useApp, useActiveServer, useActiveServerId } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { publicKeyToString } from "../lib/types";
import Message from "./Message";
import MessageInput from "./MessageInput";

export default function ThreadPanel() {
  const { dispatch } = useApp();
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const bottomRef = useRef<HTMLDivElement>(null);

  const threadChannelId = activeServer?.threadChannelId ?? null;
  const members = activeServer?.members ?? [];
  const messages = activeServer?.messages ?? {};
  const channels = activeServer?.channels ?? [];

  const memberNames: Record<string, string> = {};
  for (const m of members) {
    memberNames[publicKeyToString(m.public_key)] = m.display_name;
  }

  useEffect(() => {
    if (threadChannelId === null || !serverId) return;
    const id = threadChannelId;
    (async () => {
      try {
        const msgs = await api.fetchHistory(serverId, id);
        dispatch({ type: "SET_MESSAGES", serverId: serverId!, payload: { channelId: id, messages: msgs } });
      } catch {}
    })();
  }, [threadChannelId, serverId, dispatch]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [threadChannelId === null ? null : (messages[threadChannelId!]?.length ?? 0)]);

  if (threadChannelId === null || !serverId) return null;

  const threadChannel = channels.find((c) => c.id === threadChannelId);
  const threadMessages = messages[threadChannelId] ?? [];

  function handleBack() {
    if (serverId) dispatch({ type: "VIEW_THREAD", serverId, payload: null });
  }

  return (
    <div className="chat-panel">
      <div className="thread-back-btn" onClick={handleBack}>
        ← Back
      </div>
      <div className="channel-header">
        <span className="channel-header-name">
          # {threadChannel?.name ?? "Thread"}
        </span>
      </div>
      <div className="message-list">
        {threadMessages.map((msg) => (
          <Message key={msg.id} message={msg} memberNames={memberNames} serverId={serverId} />
        ))}
        <div ref={bottomRef} />
      </div>
      <MessageInput channelId={threadChannelId} serverId={serverId} />
    </div>
  );
}
