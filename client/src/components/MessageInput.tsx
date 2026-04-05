import { useState, useRef, KeyboardEvent } from "react";
import * as api from "../lib/tauri-bridge";
import type { FavoriteEntry } from "../lib/tauri-bridge";
import type { MemberInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";
import { useActiveServer } from "../context/ServerContext";
import FavoritesPanel from "./FavoritesPanel";
import VoiceRecorder from "./VoiceRecorder";

interface MessageInputProps {
  channelId: number;
  serverId: string;
  replyTo?: number;
  onSent?: () => void;
}

export default function MessageInput({ channelId, serverId, replyTo, onSent }: MessageInputProps) {
  const [content, setContent] = useState("");
  const [sending, setSending] = useState(false);
  const lastTypingSent = useRef(0);
  const [attachedFileId, setAttachedFileId] = useState<number | null>(null);
  const [attachedFileName, setAttachedFileName] = useState<string | null>(null);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showFavorites, setShowFavorites] = useState(false);
  const [showVoiceRecorder, setShowVoiceRecorder] = useState(false);
  const [showMentions, setShowMentions] = useState(false);
  const [mentionQuery, setMentionQuery] = useState("");
  const [mentionIndex, setMentionIndex] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const activeServer = useActiveServer();
  const members = activeServer?.members ?? [];

  const filteredMembers = members.filter(m =>
    m.display_name.toLowerCase().includes(mentionQuery)
  ).slice(0, 8);

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

  async function handleVoiceRecorded(filePath: string, _duration: number) {
    setShowVoiceRecorder(false);
    setSending(true);
    try {
      // Upload the WAV file via existing system
      const fileId = await api.uploadFile(serverId, channelId, filePath);

      // Send message with attachment
      await api.sendMessage(serverId, channelId, "", undefined, [fileId]);
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
      if (onSent) onSent();
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  }

  function insertMention(member: MemberInfo) {
    const cursorPos = textareaRef.current?.selectionStart ?? content.length;
    const textBeforeCursor = content.slice(0, cursorPos);
    const textAfterCursor = content.slice(cursorPos);
    const atPos = textBeforeCursor.lastIndexOf("@");
    const newText = textBeforeCursor.slice(0, atPos) + `@${member.display_name} ` + textAfterCursor;
    setContent(newText);
    setShowMentions(false);
  }

  function handleChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    const val = e.target.value;
    setContent(val);

    // Detect @mention
    const cursorPos = e.target.selectionStart ?? val.length;
    const textBeforeCursor = val.slice(0, cursorPos);
    const atMatch = textBeforeCursor.match(/@(\w*)$/);

    if (atMatch) {
      setMentionQuery(atMatch[1].toLowerCase());
      setShowMentions(true);
      setMentionIndex(0);
    } else {
      setShowMentions(false);
    }

    // Throttled typing indicator
    const now = Date.now();
    if (now - lastTypingSent.current > 5000 && val.trim()) {
      lastTypingSent.current = now;
      api.sendTyping(serverId, channelId).catch(() => {});
    }
  }

  function handleKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (showMentions && filteredMembers.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMentionIndex(prev => Math.min(prev + 1, filteredMembers.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMentionIndex(prev => Math.max(prev - 1, 0));
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
        e.preventDefault();
        insertMention(filteredMembers[mentionIndex]);
        return;
      }
      if (e.key === "Escape") {
        setShowMentions(false);
        return;
      }
    }
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
        {showMentions && filteredMembers.length > 0 && (
          <div className="mention-autocomplete">
            {filteredMembers.map((m, i) => (
              <div
                key={publicKeyToString(m.public_key)}
                className={`mention-autocomplete-item${i === mentionIndex ? " active" : ""}`}
                onMouseDown={(e) => { e.preventDefault(); insertMention(m); }}
              >
                <span className="mention-avatar">{m.display_name.charAt(0).toUpperCase()}</span>
                <span>{m.display_name}</span>
              </div>
            ))}
          </div>
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
        {showVoiceRecorder ? (
          <VoiceRecorder
            onRecorded={handleVoiceRecorded}
            onCancel={() => setShowVoiceRecorder(false)}
          />
        ) : (
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
            <button
              className="xp-button attach-btn"
              onClick={() => setShowVoiceRecorder(true)}
              disabled={sending}
              title="Voice Message"
            >
              Mic
            </button>
            <textarea
              ref={textareaRef}
              className="message-input"
              value={content}
              onChange={handleChange}
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
        )}
      </div>
    </div>
  );
}
