import { useEffect, useRef } from "react";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { publicKeyToString } from "../lib/types";
import Message from "./Message";
import MessageInput from "./MessageInput";

export default function ThreadPanel() {
  const { state, dispatch } = useServer();
  const { threadChannelId, members, messages } = state;
  const bottomRef = useRef<HTMLDivElement>(null);

  const memberNames: Record<string, string> = {};
  for (const m of members) {
    memberNames[publicKeyToString(m.public_key)] = m.display_name;
  }

  useEffect(() => {
    if (threadChannelId === null) return;
    const id = threadChannelId;
    (async () => {
      try {
        // subscribeChannels is handled centrally by AppShell
        const msgs = await api.fetchHistory(id);
        dispatch({ type: "SET_MESSAGES", payload: { channelId: id, messages: msgs } });
      } catch {
        // ignore
      }
    })();
  }, [threadChannelId, dispatch]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [threadChannelId === null ? null : (messages[threadChannelId]?.length ?? 0)]);

  if (threadChannelId === null) return null;

  const threadChannel = state.channels.find((c) => c.id === threadChannelId);
  const threadMessages = messages[threadChannelId] ?? [];

  function handleBack() {
    dispatch({ type: "VIEW_THREAD", payload: null });
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
          <Message key={msg.id} message={msg} memberNames={memberNames} />
        ))}
        <div ref={bottomRef} />
      </div>
      <MessageInput channelId={threadChannelId} />
    </div>
  );
}
