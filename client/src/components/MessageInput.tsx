import { useState, KeyboardEvent } from "react";
import * as api from "../lib/tauri-bridge";

interface MessageInputProps {
  channelId: number;
  replyTo?: number;
}

export default function MessageInput({ channelId, replyTo }: MessageInputProps) {
  const [content, setContent] = useState("");
  const [sending, setSending] = useState(false);

  async function handleSend() {
    const text = content.trim();
    if (!text || sending) return;
    setSending(true);
    try {
      await api.sendMessage(channelId, text, replyTo);
      setContent("");
    } catch {
      // silently ignore for now
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
      <textarea
        className="message-input"
        value={content}
        onChange={(e) => setContent(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="Type a message… (Enter to send, Shift+Enter for newline)"
        rows={1}
        disabled={sending}
      />
      <button className="xp-button" onClick={handleSend} disabled={sending || !content.trim()}>
        Send
      </button>
    </div>
  );
}
