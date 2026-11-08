import { useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import * as api from "../lib/tauri-bridge";
import { useReplySender } from "../hooks/useReplySender";
import { MAX_REPLY_CHARS } from "../lib/quickReply";
import {
  STRIP_READY_EVENT, STRIP_STATE_EVENT,
  type StripPreview, type StripState,
} from "../lib/edgeStrip";

// The view rendered inside the detached, always-on-top "strip" window (see
// src-tauri/src/edge_strip.rs). One line, in the ACTIVE THEME's look. The main
// window decides what to show (and how much a message may reveal) and pushes it
// as `strip:state`; this page only draws it and asks the main window to open a
// conversation or to send a reply.

/** How long a new message's preview stays before folding back to the chips. */
const PREVIEW_MS = 10_000;

const EMPTY: StripState = { chips: [], preview: null };

export default function EdgeStrip() {
  const [state, setState] = useState<StripState>(EMPTY);
  // The preview currently being shown (a message newer than the last one folded away).
  const [shown, setShown] = useState<StripPreview | null>(null);
  const [replying, setReplying] = useState(false);
  const seenId = useRef(0);
  const timer = useRef<number | null>(null);
  const hovering = useRef(false);
  const replyingRef = useRef(false);
  replyingRef.current = replying;

  function clearTimer() {
    if (timer.current !== null) { window.clearTimeout(timer.current); timer.current = null; }
  }
  function armFold() {
    clearTimer();
    timer.current = window.setTimeout(() => {
      if (hovering.current || replyingRef.current) { armFold(); return; }
      setShown(null);
    }, PREVIEW_MS);
  }
  function endReply() {
    setReplying(false);
    api.stripFocus(false).catch(() => {});
    armFold();
  }

  // Receive the main window's state; ask for it once we are listening.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let unlistenSettings: (() => void) | null = null;
    let cancelled = false;
    listen<StripState>(STRIP_STATE_EVENT, async (e) => {
      try {
        // A theme change made while the app runs should show here too.
        const { id: themeId, css } = await api.getActiveTheme();
        const style = document.getElementById("active-theme");
        if (style && document.documentElement.dataset.theme !== themeId) {
          style.textContent = css;
          document.documentElement.dataset.theme = themeId;
        }
      } catch { /* keep the current look */ }
      const next = e.payload ?? EMPTY;
      setState(next);
      if (next.chips.length === 0) { setShown(null); setReplying(false); }
      if (next.preview && next.preview.id > seenId.current) {
        seenId.current = next.preview.id;
        setShown(next.preview);
        setReplying(false);
        armFold();
      }
    }).then((u) => { if (cancelled) u(); else unlisten = u; });
    // Settings (dock/size/visibility) changed: re-place the window.
    listen("strip:settings-changed", () => { void api.stripLayout(hasContentRef.current).catch(() => {}); })
      .then((u) => { if (cancelled) u(); else unlistenSettings = u; });
    void emit(STRIP_READY_EVENT, {});
    return () => { cancelled = true; unlisten?.(); unlistenSettings?.(); clearTimer(); };
  }, []);

  const hasContent = state.chips.length > 0 || shown !== null;
  const hasContentRef = useRef(hasContent);
  hasContentRef.current = hasContent;
  // Place / show / hide the native window to match what there is to show.
  useEffect(() => { api.stripLayout(hasContent).catch(() => {}); }, [hasContent]);

  function open(serverId: string | null, channelId: number | null) {
    api.stripOpen(serverId, channelId).catch(() => {});
  }

  // --- the three faces of the strip -----------------------------------------
  let body;
  if (shown && replying && shown.reply) {
    body = (
      <ReplyLine
        title={shown.title}
        target={shown.reply}
        onSent={() => {
          const t = shown.reply!;
          void emit("strip:replied", { server_id: t.server_id, channel_id: t.channel_id });
          setReplying(false);
          setShown(null);
          api.stripFocus(false).catch(() => {});
        }}
        onCancel={endReply}
      />
    );
  } else if (shown) {
    body = (
      <>
        <button className="edge-strip-text" type="button" onClick={() => open(shown.reply?.server_id ?? null, shown.reply?.channel_id ?? null)}>
          <span className="edge-strip-title">{shown.title}</span>
          <span className="edge-strip-body">{shown.body}</span>
        </button>
        {shown.reply && (
          <button
            className="edge-strip-btn"
            type="button"
            onClick={() => {
              clearTimer();
              setReplying(true);
              // Unfocusable until now: make it focusable, then focus it.
              api.stripFocus(true).catch(() => {});
            }}
          >
            Reply
          </button>
        )}
        <button className="edge-strip-close" type="button" aria-label="Dismiss" onClick={() => setShown(null)}>×</button>
      </>
    );
  } else if (state.chips.length > 0) {
    body = (
      <>
        {state.chips.map((c) => (
          <button key={c.key} className="edge-strip-chip" type="button" onClick={() => open(c.server_id, c.channel_id)}>
            <span className="edge-strip-chip-label">{c.label}</span>
            <span className="edge-strip-count">{c.count}</span>
          </button>
        ))}
        <span className="edge-strip-spacer" />
      </>
    );
  } else {
    body = <span className="edge-strip-idle">No new messages</span>;
  }

  return (
    <div
      className="edge-strip"
      onMouseEnter={() => { hovering.current = true; }}
      onMouseLeave={() => { hovering.current = false; if (shown && !replying) armFold(); }}
    >
      <button className="edge-strip-av" type="button" aria-label="Open Farder" onClick={() => open(null, null)}>F</button>
      {body}
    </div>
  );
}

/** One-line reply: a box in place of the preview. Esc cancels, Enter sends. */
function ReplyLine({ title, target, onSent, onCancel }: {
  title: string;
  target: { server_id: string; channel_id: number };
  onSent: () => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState("");
  const { state, error, send } = useReplySender(onSent);
  const busy = state === "sending" || state === "sent";
  return (
    <>
      <input
        className="edge-strip-input"
        autoFocus
        maxLength={MAX_REPLY_CHARS}
        value={text}
        disabled={busy}
        aria-label={`Reply to ${title}`}
        placeholder="Type a reply and press Enter"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") { e.preventDefault(); void send(target, text.trim()); }
          else if (e.key === "Escape") { e.preventDefault(); onCancel(); }
        }}
        onBlur={() => { if (!busy) onCancel(); }}
      />
      {state === "sending" && <span className="edge-strip-status">Sending...</span>}
      {state === "sent" && <span className="edge-strip-status">Sent</span>}
      {state === "error" && <span className="edge-strip-status edge-strip-error" title={error ?? ""}>{error}</span>}
    </>
  );
}
