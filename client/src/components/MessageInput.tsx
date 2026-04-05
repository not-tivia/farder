import { useState, KeyboardEvent } from "react";
import * as api from "../lib/tauri-bridge";
import type { FavoriteEntry } from "../lib/tauri-bridge";
import FavoritesPanel from "./FavoritesPanel";

interface MessageInputProps {
  channelId: number;
  serverId: string;
  replyTo?: number;
}

export default function MessageInput({ channelId, serverId, replyTo }: MessageInputProps) {
  const [content, setContent] = useState("");
  const [sending, setSending] = useState(false);
  const [attachedFileId, setAttachedFileId] = useState<number | null>(null);
  const [attachedFileName, setAttachedFileName] = useState<string | null>(null);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showFavorites, setShowFavorites] = useState(false);

  async function handleAttach() {
    setError(null);
    const path = await api.pickFile();
    if (!path) return;
    const fileName = path.split(/[/\\]/).pop() ?? "file";
    setAttachedFileName(fileName);
    setUploading(true);
    try {
      const fileId = await api.uploadFile(serverId, channelId, path);
      setAttachedFileId(fileId);
    } catch (e) {
      setError(String(e));
      setAttachedFileName(null);
      setAttachedFileId(null);
    } finally {
      setUploading(false);
    }
  }

  function handleRemoveAttachment() {
    setAttachedFileId(null);
    setAttachedFileName(null);
    setError(null);
  }

  async function handleFavoriteSelect(fav: FavoriteEntry) {
    setShowFavorites(false);
    setSending(true);
    try {
      if (fav.original_url) {
        const fileId = await api.fetchUrl(serverId, fav.original_url, channelId);
        await api.sendMessage(serverId, channelId, "", undefined, [fileId]);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  }

  const imageUrlRegex = /https?:\/\/[^\s]+\.(?:png|jpg|jpeg|gif|webp)(?:\?[^\s]*)?/gi;

  async function handleSend() {
    const text = content.trim();
    if ((!text && !attachedFileId) || sending) return;
    setSending(true);
    setError(null);
    try {
      const attachments: number[] = attachedFileId ? [attachedFileId] : [];

      // Auto-fetch image URLs found in the message text
      const urls = text.match(imageUrlRegex) || [];
      for (const url of urls) {
        try {
          const fileId = await api.fetchUrl(serverId, url, channelId);
          attachments.push(fileId);
        } catch {
          // Failed to fetch — leave the URL as plain text
        }
      }

      await api.sendMessage(
        serverId,
        channelId,
        text,
        replyTo,
        attachments.length > 0 ? attachments : undefined,
      );
      setContent("");
      setAttachedFileId(null);
      setAttachedFileName(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  }

  function handleKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  return (
    <div className="message-input-area">
      <div className="message-input-wrapper">
        {showFavorites && (
          <FavoritesPanel onSelect={handleFavoriteSelect} onClose={() => setShowFavorites(false)} />
        )}
        {(attachedFileName || uploading) && (
          <div className="attachment-preview">
            {uploading ? (
              <span>Uploading...</span>
            ) : (
              <>
                <span className="attachment-preview-name">{attachedFileName}</span>
                <button className="attachment-remove-btn" onClick={handleRemoveAttachment} title="Remove attachment">
                  x
                </button>
              </>
            )}
          </div>
        )}
        {error && <div className="error-text" style={{ padding: "2px 4px" }}>{error}</div>}
        <div className="message-input-row">
          <button
            className="xp-button attach-btn"
            onClick={() => setShowFavorites(!showFavorites)}
            disabled={sending}
            title="Favorites"
          >
            *
          </button>
          <button
            className="xp-button attach-btn"
            onClick={handleAttach}
            disabled={sending || uploading}
            title="Attach file"
          >
            +
          </button>
          <textarea
            className="message-input"
            value={content}
            onChange={(e) => setContent(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Type a message… (Enter to send, Shift+Enter for newline)"
            rows={1}
            disabled={sending}
          />
          <button
            className="xp-button"
            onClick={handleSend}
            disabled={sending || uploading || (!content.trim() && !attachedFileId)}
          >
            Send
          </button>
        </div>
      </div>
    </div>
  );
}
