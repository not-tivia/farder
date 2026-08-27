import { useEffect, useRef, useState } from "react";
let cachedOwnPk: string | null = null;
import { useApp, useActiveServer, useActiveServerId } from "../context/ServerContext";
import { publicKeyToString, flattenMessageInfoV2, isE2eeChannel } from "../lib/types";
import type { MessageInfo } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import Message from "./Message";
import MessageInput from "./MessageInput";
import ThreadPanel from "./ThreadPanel";
import ActiveWidgetsBar from "./ActiveWidgetsBar";
import { openMessageSearch } from "./AppShell";

export default function ChatPanel() {
  const { dispatch } = useApp();
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const bottomRef = useRef<HTMLDivElement>(null);
  const [ownPk, setOwnPk] = useState(cachedOwnPk);
  const [loadingMore, setLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const [replyTo, setReplyTo] = useState<MessageInfo | null>(null);

  const members = activeServer?.members ?? [];
  const channels = activeServer?.channels ?? [];
  const messages = activeServer?.messages ?? {};
  const dms = activeServer?.dms ?? [];
  const currentChannelId = activeServer?.currentChannelId ?? null;
  const threadChannelId = activeServer?.threadChannelId ?? null;

  useEffect(() => {
    if (!cachedOwnPk) {
      api.getPublicKey().then(pk => { cachedOwnPk = pk; setOwnPk(pk); });
    }
  }, []);

  const memberNames: Record<string, string> = {};
  for (const m of members) {
    memberNames[publicKeyToString(m.public_key)] = m.display_name;
  }

  const typingUsers = activeServer?.typingUsers?.[currentChannelId!] ?? [];
  const othersTyping = typingUsers.filter(t => t.publicKey !== ownPk);

  const currentChannel = currentChannelId !== null
    ? channels.find((c) => c.id === currentChannelId)
      ?? dms.find((d) => d.channel.id === currentChannelId)?.channel
      ?? null
    : null;

  const channelMessages = currentChannelId !== null ? (messages[currentChannelId] ?? []) : [];

  const highlightMessageId = activeServer?.highlightMessageId ?? null;
  useEffect(() => {
    if (highlightMessageId === null) return;
    // Wait one tick so the message is in the DOM if the channel just switched.
    const t = setTimeout(() => {
      const el = document.getElementById(`msg-${highlightMessageId}`);
      el?.scrollIntoView({ block: "center", behavior: "smooth" });
    }, 50);
    // Clear after the flash animation completes (1.2s in CSS).
    const clear = setTimeout(() => {
      if (!serverId) return;
      dispatch({ type: "HIGHLIGHT_MESSAGE", serverId, payload: { messageId: null } });
    }, 1300);
    return () => {
      clearTimeout(t);
      clearTimeout(clear);
    };
  }, [highlightMessageId, serverId, dispatch]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [channelMessages.length]);

  useEffect(() => {
    setHasMore(true);
    setReplyTo(null);
  }, [currentChannelId]);

  async function handleScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    if (el.scrollTop === 0 && !loadingMore && hasMore && currentChannelId && serverId) {
      const oldest = channelMessages[0];
      if (!oldest) return;
      setLoadingMore(true);
      try {
        const older = currentChannel?.class === "E2ee"
          ? flattenMessageInfoV2(await api.fetchHistoryV2(serverId, currentChannelId, oldest.id, 50))
          : await api.fetchHistory(serverId, currentChannelId, oldest.id, 50);
        if (older.length === 0) {
          setHasMore(false);
        } else {
          dispatch({ type: "PREPEND_MESSAGES", serverId, payload: { channelId: currentChannelId, messages: older.reverse() } });
        }
      } catch {}
      setLoadingMore(false);
    }
  }

  if (!activeServer || !serverId) {
    return (
      <div className="chat-panel">
        <div className="message-list-placeholder">Select a server to start chatting.</div>
      </div>
    );
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
        <span className="channel-header-name">
          {currentChannel?.channel_type === "Dm"
            ? dms.find(d => d.channel.id === currentChannelId)?.participant.display_name ?? "DM"
            : `# ${currentChannel?.name ?? "unknown"}`}
        </span>
        {isE2eeChannel(currentChannel) && (
          <span className="channel-header-e2ee" title="End-to-end encrypted channel">🔒 Encrypted</span>
        )}
        {currentChannel?.topic && (
          <span className="channel-header-topic">{currentChannel.topic}</span>
        )}
        {currentChannel?.channel_type === "Dm" && (
          <button className="xp-button" style={{ fontSize: 10, marginLeft: 8, padding: "2px 8px" }}
            onClick={() => {
              dispatch({ type: "OPEN_DM_PANEL", serverId, payload: currentChannelId! });
              const firstServerCh = channels.find(c => c.channel_type !== "Dm" && c.channel_type !== "Thread");
              if (firstServerCh) {
                dispatch({ type: "SELECT_CHANNEL", serverId, payload: firstServerCh.id });
              }
            }}
          >Pop Out</button>
        )}
        <button
          className="search-toggle"
          onClick={() => openMessageSearch()}
          title="Search messages (Ctrl+K)"
          aria-label="Search messages"
        >
          🔍
        </button>
      </div>
      <ActiveWidgetsBar serverId={serverId} channelId={currentChannelId} />
      <div className="message-list" onScroll={handleScroll}>
        {loadingMore && <div className="load-more-indicator">Loading...</div>}
        {channelMessages.map((msg, i) => {
          const prev = i > 0 ? channelMessages[i - 1] : null;
          const sameAuthor = prev &&
            JSON.stringify(prev.author.bytes) === JSON.stringify(msg.author.bytes);
          const withinWindow = prev &&
            (msg.timestamp - prev.timestamp) < 300;
          const grouped = !!(sameAuthor && withinWindow);
          return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} serverId={serverId} highlighted={msg.id === highlightMessageId} sealedDecrypt={activeServer.sealedDecrypts?.[msg.id]} onReply={(msg) => setReplyTo(msg)} />;
        })}
        <div ref={bottomRef} />
      </div>
      {othersTyping.length > 0 && (
        <div className="typing-indicator">
          {othersTyping.length === 1
            ? `${memberNames[othersTyping[0].publicKey] ?? "Someone"} is typing...`
            : othersTyping.length === 2
              ? `${memberNames[othersTyping[0].publicKey] ?? "Someone"} and ${memberNames[othersTyping[1].publicKey] ?? "someone"} are typing...`
              : `${othersTyping.length} people are typing...`
          }
        </div>
      )}
      {replyTo && (
        <div className="reply-preview">
          <span>Replying to <strong>{memberNames[publicKeyToString(replyTo.author)] ?? "someone"}</strong></span>
          <span className="reply-preview-text">{replyTo.content.slice(0, 80)}{replyTo.content.length > 80 ? "..." : ""}</span>
          <button className="reply-cancel" onClick={() => setReplyTo(null)}>X</button>
        </div>
      )}
      <MessageInput channelId={currentChannelId} serverId={serverId} replyTo={replyTo?.id} onSent={() => setReplyTo(null)} />
    </div>
  );
}
