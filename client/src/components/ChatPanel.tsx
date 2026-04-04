import { useEffect, useRef } from "react";
import { useServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import Message from "./Message";
import MessageInput from "./MessageInput";
import ThreadPanel from "./ThreadPanel";

export default function ChatPanel() {
  const { state } = useServer();
  const { currentChannelId, threadChannelId, messages, channels, members } = state;
  const bottomRef = useRef<HTMLDivElement>(null);

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
      <div className="message-list">
        {channelMessages.map((msg) => (
          <Message key={msg.id} message={msg} memberNames={memberNames} />
        ))}
        <div ref={bottomRef} />
      </div>
      <MessageInput channelId={currentChannelId} />
    </div>
  );
}
