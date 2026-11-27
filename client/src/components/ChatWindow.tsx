import { useEffect, useRef, useState } from "react";
import { emit } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import * as api from "../lib/tauri-bridge";
import { useReplySender } from "../hooks/useReplySender";
import { MAX_REPLY_CHARS } from "../lib/quickReply";
import { shortClock } from "../lib/density";
import { CHAT_READ_EVENT, CHAT_READY_EVENT, CHAT_STATE_EVENT, type ChatState } from "../lib/buddy";

// The view rendered inside one chat window (see src-tauri/src/buddy.rs): the recent
// messages of one conversation and a box to reply, in the ACTIVE THEME's look. The
// main window pushes the transcript (`chat:state`, addressed to this window only);
// this page draws it, asks the main window to mark it read, and sends a line by
// ASKING the main window (the same `notify:reply` path as the pop-up), so a DM is
// encrypted before it leaves and an encrypted channel is refused.

export default function ChatWindow() {
  const params = new URLSearchParams(window.location.search);
  const serverId = params.get("s") ?? "";
  const channelId = Number(params.get("c"));
  const [chat, setChat] = useState<ChatState | null | undefined>(undefined);
  const [text, setText] = useState("");
  const logRef = useRef<HTMLDivElement>(null);
  const markedRef = useRef(0);
  const chatRef = useRef<ChatState | null | undefined>(undefined);
  chatRef.current = chat;
  const resetRef = useRef<() => void>(() => {});
  const { state, error, send, reset } = useReplySender(() => { setText(""); resetRef.current(); });
  resetRef.current = reset;
  const busy = state === "sending" || state === "sent";

  function markRead() {
    const c = chatRef.current;
    if (!c || c.last_id <= markedRef.current) return;
    markedRef.current = c.last_id;
    const label = getCurrentWebviewWindow().label;
    void emit(CHAT_READ_EVENT, { label, server_id: serverId, channel_id: channelId, last_id: c.last_id });
  }

  useEffect(() => {
    const win = getCurrentWebviewWindow();
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    win.listen<ChatState | null>(CHAT_STATE_EVENT, async (e) => {
      try {
        const { id: themeId, css } = await api.getActiveTheme();
        const style = document.getElementById("active-theme");
        if (style && document.documentElement.dataset.theme !== themeId) {
          style.textContent = css;
          document.documentElement.dataset.theme = themeId;
        }
      } catch { /* keep the current look */ }
      setChat(e.payload ?? null);
    }).then((u) => { if (cancelled) u(); else unlisten = u; });
    // Ask for the conversation, and keep asking until it arrives.
    const ask = () => void emit(CHAT_READY_EVENT, { label: win.label, server_id: serverId, channel_id: channelId });
    ask();
    const retry = window.setInterval(() => { if (chatRef.current === undefined) ask(); else window.clearInterval(retry); }, 2000);
    const onFocus = () => markRead();
    window.addEventListener("focus", onFocus);
    return () => { cancelled = true; unlisten?.(); window.clearInterval(retry); window.removeEventListener("focus", onFocus); };
  }, []);

  // Keep the newest message in view, and count what is on screen as read while
  // the window is the one being looked at.
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
    if (document.hasFocus()) markRead();
  }, [chat]);

  return (
    <div className="mini-chat">
      <div className="mini-chat-head">{chat === undefined ? "..." : chat === null ? "Conversation not found" : chat.title}</div>
      <div className="mini-chat-log" ref={logRef}>
        {chat === undefined && <div className="mini-chat-note">Loading... (if this stays, make sure the main Farder window is open and unlocked)</div>}
        {chat === null && <div className="mini-chat-note">This conversation is not available any more. Open Farder and check you are still connected.</div>}
        {chat && chat.lines.length === 0 && <div className="mini-chat-note">No messages yet.</div>}
        {chat && chat.lines.map((l) => (
          <div key={l.id} className={`mini-chat-line${l.mine ? " mine" : ""}`}>
            <span className="mini-chat-time">{shortClock(l.time)}</span>
            <span className="mini-chat-author">{l.author}</span>
            <span className="mini-chat-text">{l.text}</span>
          </div>
        ))}
      </div>
      <div className="mini-chat-foot">
        {chat && !chat.can_send && <div className="mini-chat-note">{chat.reason}</div>}
        <input
          className="connect-input mini-chat-input"
          maxLength={MAX_REPLY_CHARS}
          value={text}
          disabled={busy || !chat || !chat.can_send}
          placeholder={chat?.can_send ? "Type a message and press Enter" : ""}
          aria-label="Message"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && chat) { e.preventDefault(); void send({ server_id: chat.server_id, channel_id: chat.channel_id }, text.trim()); }
          }}
        />
        {state === "sending" && <div className="mini-chat-note">Sending...</div>}
        {state === "error" && <div className="mini-chat-note mini-chat-error">{error}</div>}
      </div>
    </div>
  );
}
