import { useEffect, useRef, useState } from "react";
import { useServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import Message from "./Message";
import MessageInput from "./MessageInput";
import ThreadPanel from "./ThreadPanel";

export default function ChatPanel() {
  const { state, dispatch } = useServer();
  const { currentChannelId, threadChannelId, messages, channels, members } = state;
  const bottomRef = useRef<HTMLDivElement>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(true);

  const memberNames: Record<string, string> = {};
  for (const m of members) {
    memberNames[publicKeyToString(m.public_key)] = m.display_name;
  }

  const currentChannel = currentChannelId !== null
    ? channels.find((c) => c.id === currentChannelId)
    : null;

  const channelMessages = currentChannelId !== null ? (messages[currentChannelId] ?? []) : [];

  // Auto-scroll to bottom when new messages arrive in the current channel
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [channelMessages.length]);

  // Reset hasMore when switching channels
  useEffect(() => {
    setHasMore(true);
  }, [currentChannelId]);

  async function handleScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    if (el.scrollTop === 0 && !loadingMore && hasMore && currentChannelId) {
      const oldest = channelMessages[0];
      if (!oldest) return;
      setLoadingMore(true);
      try {
        const older = await api.fetchHistory(currentChannelId, oldest.id, 50);
        if (older.length === 0) {
          setHasMore(false);
        } else {
          dispatch({ type: "PREPEND_MESSAGES", payload: { channelId: currentChannelId, messages: older.reverse() } });
        }
      } catch {
        // ignore
      }
      setLoadingMore(false);
    }
  }

  if (threadChannelId !== null) {
    return <ThreadPanel />;
  }

  if (currentChannelId === null) {
    return (
      <div className="chat-panel">
        <div className="message-list-placeholder">
          Select a channel to start chatting.
        </div>
      </div>
    );
  }

  return (
    <div className="chat-panel">
      <div className="channel-header">
        <span className="channel-header-name"># {currentChannel?.name ?? "unknown"}</span>
        {currentChannel?.topic && (
          <span className="channel-header-topic">{currentChannel.topic}</span>
        )}
      </div>
      <div className="message-list" onScroll={handleScroll}>
        {loadingMore && <div className="load-more-indicator">Loading...</div>}
        {channelMessages.map((msg, i) => {
          const prev = i > 0 ? channelMessages[i - 1] : null;
          const sameAuthor = prev &&
            JSON.stringify(prev.author.bytes) === JSON.stringify(msg.author.bytes);
          const withinWindow = prev &&
            (new Date(msg.timestamp).getTime() - new Date(prev.timestamp).getTime()) < 300_000;
          const grouped = !!(sameAuthor && withinWindow);
          return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} />;
        })}
        <div ref={bottomRef} />
      </div>
      <MessageInput channelId={currentChannelId} />
    </div>
  );
}
