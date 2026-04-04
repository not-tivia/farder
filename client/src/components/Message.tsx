import { useState, useEffect } from "react";
import type { MessageInfo, AttachmentInfo } from "../lib/types";
import { publicKeyToString, isDeletedUser } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { useServer } from "../context/ServerContext";
import ReactionPicker from "./ReactionPicker";

interface MessageProps {
  message: MessageInfo;
  memberNames: Record<string, string>;
  grouped?: boolean;
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

function formatTimestamp(ts: number): string {
  try {
    const date = new Date(ts * 1000); // Unix seconds → JS milliseconds
    const now = new Date();
    const isToday = date.toDateString() === now.toDateString();
    const yesterday = new Date(now);
    yesterday.setDate(yesterday.getDate() - 1);
    const isYesterday = date.toDateString() === yesterday.toDateString();

    const time = date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

    if (isToday) return `Today at ${time}`;
    if (isYesterday) return `Yesterday at ${time}`;
    return `${date.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" })} at ${time}`;
  } catch {
    return String(ts);
  }
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function Message({ message, memberNames, grouped = false }: MessageProps) {
  const { dispatch } = useServer();
  const [showPicker, setShowPicker] = useState(false);
  const [reacting, setReacting] = useState(false);

  const deleted = isDeletedUser(message.author);
  const pkStr = publicKeyToString(message.author);
  const displayName = deleted
    ? "Deleted User"
    : (memberNames[pkStr] ?? pkStr.slice(0, 16) + "…");
  const color = deleted ? "#999" : authorColor(pkStr);

  async function handleReactionClick(emoji: string, alreadyMe: boolean) {
    if (reacting) return;
    setReacting(true);
    try {
      if (alreadyMe) {
        await api.removeReaction(message.id, emoji);
      } else {
        await api.addReaction(message.id, emoji);
      }
    } catch {
      // ignore
    } finally {
      setReacting(false);
    }
  }

  async function handlePickerSelect(emoji: string) {
    if (reacting) return;
    setShowPicker(false);
    setReacting(true);
    try {
      await api.addReaction(message.id, emoji);
    } catch {
      // ignore
    } finally {
      setReacting(false);
    }
  }

  function handleThreadClick() {
    if (message.thread_id !== null) {
      dispatch({ type: "VIEW_THREAD", payload: message.thread_id });
    }
  }

  return (
    <div className={`message${grouped ? " grouped" : ""}`}>
      {grouped && (
        <span className="grouped-timestamp">
          {new Date(message.timestamp * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}
        </span>
      )}
      {!grouped && (
        <div className="message-header">
          <span className="message-author" style={{ color }}>
            {displayName}
          </span>
          <span className="message-timestamp">{formatTimestamp(message.timestamp)}</span>
          {message.edited_at && <span className="message-edited">(edited)</span>}
        </div>
      )}
      <div className={`message-content${deleted ? " deleted-content" : ""}`}>
        {deleted ? <em>This message has been deleted.</em> : message.content}
      </div>

      {message.attachments.length > 0 && (
        <div className="message-attachments">
          {message.attachments.map((att) => (
            <AttachmentDisplay key={att.id} attachment={att} />
          ))}
        </div>
      )}

      <div className={`reaction-bar${message.reactions.length === 0 ? " hover-only" : ""}`}>
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

      {message.thread_id !== null && (
        <div className="thread-link" onClick={handleThreadClick}>
          💬 {message.thread_message_count > 0 ? `${message.thread_message_count} replies` : "View thread"}
        </div>
      )}
    </div>
  );
}

function AttachmentDisplay({ attachment }: { attachment: AttachmentInfo }) {
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);
  const isImage = attachment.mime_type.startsWith("image/");

  useEffect(() => {
    if (isImage) {
      api.downloadFile(attachment.file_id).then((r) => {
        if (r.data_url) setImageUrl(r.data_url);
      }).catch(() => {});
    }
  }, [attachment.file_id, isImage]);

  async function handleDownload() {
    if (downloading) return;
    setDownloading(true);
    try {
      const result = await api.downloadFile(attachment.file_id);
      if (result.saved_path) {
        alert(`Saved to ${result.saved_path}`);
      }
    } catch (e) {
      alert(`Download failed: ${e}`);
    } finally {
      setDownloading(false);
    }
  }

  if (isImage && imageUrl) {
    return (
      <div className="attachment-image">
        <img src={imageUrl} alt={attachment.name} style={{ maxWidth: 400, maxHeight: 300, borderRadius: 3 }} />
        <div className="attachment-name">{attachment.name} ({formatSize(attachment.size)})</div>
      </div>
    );
  }

  return (
    <div className="attachment-item" onClick={handleDownload} style={{ cursor: "pointer" }}>
      <span>📎</span>
      <span>{attachment.name} ({formatSize(attachment.size)})</span>
      {downloading && <span> downloading...</span>}
    </div>
  );
}
