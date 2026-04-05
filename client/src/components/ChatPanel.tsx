import { useEffect, useRef, useState } from "react";
import { useServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import type { MessageInfo } from "../lib/types";
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
  const [showSearch, setShowSearch] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<MessageInfo[] | null>(null);
  const [searching, setSearching] = useState(false);

  const memberNames: Record<string, string> = {};
  for (const m of members) {
    memberNames[publicKeyToString(m.public_key)] = m.display_name;
  }

  const currentChannel = currentChannelId !== null
    ? channels.find((c) => c.id === currentChannelId)
      ?? state.dms.find((d) => d.channel.id === currentChannelId)?.channel
      ?? null
    : null;

  const channelMessages = currentChannelId !== null ? (messages[currentChannelId] ?? []) : [];

  // Auto-scroll to bottom when new messages arrive in the current channel
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [channelMessages.length]);

  // Reset hasMore and search when switching channels
  useEffect(() => {
    setHasMore(true);
    setShowSearch(false);
    setSearchQuery("");
    setSearchResults(null);
  }, [currentChannelId]);

  async function handleSearch() {
    if (!searchQuery.trim() || !currentChannelId) return;
    setSearching(true);
    try {
      const results = await api.searchMessages(searchQuery.trim(), currentChannelId);
      setSearchResults(results);
    } catch {
      // ignore
    }
    setSearching(false);
  }

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
        <span className="channel-header-name">
          {currentChannel?.channel_type === "Dm"
            ? state.dms.find(d => d.channel.id === currentChannelId)?.participant.display_name ?? "DM"
            : `# ${currentChannel?.name ?? "unknown"}`}
        </span>
        {currentChannel?.topic && (
          <span className="channel-header-topic">{currentChannel.topic}</span>
        )}
        {currentChannel?.channel_type === "Dm" && (
          <button className="xp-button" style={{ fontSize: 10, marginLeft: 8, padding: "2px 8px" }}
            onClick={() => {
              dispatch({ type: "OPEN_DM_PANEL", payload: currentChannelId! });
              const firstServerCh = state.channels.find(c => c.channel_type !== "Dm" && c.channel_type !== "Thread");
              if (firstServerCh) {
                dispatch({ type: "SELECT_CHANNEL", payload: firstServerCh.id });
              }
            }}
          >Pop Out</button>
        )}
        <button
          className="search-toggle"
          onClick={() => setShowSearch(!showSearch)}
          title="Search messages"
        >
          ?
        </button>
      </div>
      {showSearch && (
        <div className="search-bar">
          <input
            className="search-input"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") handleSearch(); }}
            placeholder="Search messages..."
            autoFocus
          />
          <button className="xp-button" onClick={handleSearch} disabled={searching}>
            {searching ? "..." : "Search"}
          </button>
          <button className="xp-button" onClick={() => { setShowSearch(false); setSearchResults(null); setSearchQuery(""); }}>
            X
          </button>
        </div>
      )}
      <div className="message-list" onScroll={handleScroll}>
        {loadingMore && <div className="load-more-indicator">Loading...</div>}
        {channelMessages.map((msg, i) => {
          const prev = i > 0 ? channelMessages[i - 1] : null;
          const sameAuthor = prev &&
            JSON.stringify(prev.author.bytes) === JSON.stringify(msg.author.bytes);
          const withinWindow = prev &&
            (msg.timestamp - prev.timestamp) < 300; // 5 minutes in seconds
          const grouped = !!(sameAuthor && withinWindow);
          return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} />;
        })}
        <div ref={bottomRef} />
      </div>
      {searchResults && (
        <div className="search-results">
          <div className="search-results-header">
            {searchResults.length} result{searchResults.length !== 1 ? "s" : ""} for "{searchQuery}"
            <button className="xp-button" onClick={() => setSearchResults(null)} style={{ fontSize: 10, padding: "1px 6px" }}>Close</button>
          </div>
          <div className="search-results-list">
            {searchResults.map((msg) => (
              <Message key={msg.id} message={msg} memberNames={memberNames} grouped={false} />
            ))}
            {searchResults.length === 0 && <div className="search-no-results">No messages found.</div>}
          </div>
        </div>
      )}
      <MessageInput channelId={currentChannelId} />
    </div>
  );
}
