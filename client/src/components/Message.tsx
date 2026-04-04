import { useState } from "react";
import type { MessageInfo } from "../lib/types";
import { publicKeyToString, isDeletedUser } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { useServer } from "../context/ServerContext";
import ReactionPicker from "./ReactionPicker";

interface MessageProps {
  message: MessageInfo;
  memberNames: Record<string, string>;
}

/** Derive a deterministic color from a string (public key or name). */
function authorColor(key: string): string {
  let hash = 0;
  for (let i = 0; i < key.length; i++) {
    hash = (hash * 31 + key.charCodeAt(i)) >>> 0;
  }
  const hue = hash % 360;
  return `hsl(${hue}, 65%, 38%)`;
}

function formatTimestamp(ts: string): string {
  try {
    const d = new Date(ts);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return ts;
  }
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function Message({ message, memberNames }: MessageProps) {
  const { dispatch } = useServer();
  const [showPicker, setShowPicker] = useState(false);

  const deleted = isDeletedUser(message.author);
  const pkStr = publicKeyToString(message.author);
  const displayName = deleted
    ? "Deleted User"
    : (memberNames[pkStr] ?? pkStr.slice(0, 16) + "…");
  const color = deleted ? "#999" : authorColor(pkStr);

  async function handleReactionClick(emoji: string, alreadyMe: boolean) {
    try {
      if (alreadyMe) {
        await api.removeReaction(message.id, emoji);
      } else {
        await api.addReaction(message.id, emoji);
      }
    } catch {
      // ignore
    }
  }

  async function handlePickerSelect(emoji: string) {
    setShowPicker(false);
    try {
      await api.addReaction(message.id, emoji);
    } catch {
      // ignore
    }
  }

  function handleThreadClick() {
    if (message.thread_id !== null) {
      dispatch({ type: "VIEW_THREAD", payload: message.thread_id });
    }
  }

  return (
    <div className="message">
      <div className="message-header">
        <span className="message-author" style={{ color }}>
          {displayName}
        </span>
        <span className="message-timestamp">{formatTimestamp(message.timestamp)}</span>
        {message.edited_at && <span className="message-edited">(edited)</span>}
      </div>
      <div className={`message-content${deleted ? " deleted-content" : ""}`}>
        {deleted ? <em>This message has been deleted.</em> : message.content}
      </div>

      {message.attachments.length > 0 && (
        <div className="message-attachments">
          {message.attachments.map((att) => (
            <div key={att.id} className="attachment-item">
              <span>📎</span>
              <span>
                {att.name} ({formatSize(att.size)})
              </span>
            </div>
          ))}
        </div>
      )}

      {(message.reactions.length > 0 || true) && (
        <div className="reaction-bar">
          {message.reactions.map((r) => (
            <button
              key={r.emoji}
              className={`reaction${r.me ? " me" : ""}`}
              onClick={() => handleReactionClick(r.emoji, r.me)}
              title={`${r.emoji} ${r.count}`}
            >
              {r.emoji}
              <span className="reaction-count">{r.count}</span>
            </button>
          ))}
          <div style={{ position: "relative" }}>
            <button
              className="reaction-add-btn"
              onClick={() => setShowPicker((p) => !p)}
              title="Add reaction"
            >
              +
            </button>
            {showPicker && (
              <ReactionPicker onSelect={handlePickerSelect} />
            )}
          </div>
        </div>
      )}

      {message.thread_id !== null && (
        <div className="thread-link" onClick={handleThreadClick}>
          💬 {message.thread_message_count > 0 ? `${message.thread_message_count} replies` : "View thread"}
        </div>
      )}
    </div>
  );
}
