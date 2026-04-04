import { useState, KeyboardEvent } from "react";
import * as api from "../lib/tauri-bridge";

interface MessageInputProps {
  channelId: number;
  replyTo?: number;
}

export default function MessageInput({ channelId, replyTo }: MessageInputProps) {
  const [content, setContent] = useState("");
  const [sending, setSending] = useState(false);
  const [attachedFileId, setAttachedFileId] = useState<number | null>(null);
  const [attachedFileName, setAttachedFileName] = useState<string | null>(null);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleAttach() {
    setError(null);
    const path = await api.pickFile();
    if (!path) return;
    const fileName = path.split(/[/\\]/).pop() ?? "file";
    setAttachedFileName(fileName);
    setUploading(true);
    try {
      const fileId = await api.uploadFile(channelId, path);
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

  async function handleSend() {
    const text = content.trim();
    if ((!text && !attachedFileId) || sending) return;
    setSending(true);
    setError(null);
    try {
      await api.sendMessage(
        channelId,
        text,
        replyTo,
        attachedFileId ? [attachedFileId] : undefined,
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
