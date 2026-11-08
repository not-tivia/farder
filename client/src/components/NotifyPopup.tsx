import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../lib/tauri-bridge";
import { useLinkEmbed } from "../hooks/useLinkEmbed";
import { useProxiedMedia } from "../hooks/useProxiedMedia";

// The view rendered inside the detached, always-on-top "notify" window (see
// src-tauri/src/notify_popup.rs). It shows the ACTIVE THEME's look, holds the
// text only in memory, and fetches nothing: the only input is the local
// `notify:push` event the Rust side emits.

interface Card {
  id: number;
  title: string;
  body: string;
  /** A direct video/image link to preview (only sent for a trusted sender). */
  mediaUrl: string | null;
}

// Most cards on screen: 4 plain ones, but 3 when any carries a media preview, so
// the stack stays inside notify_popup::MAX_HEIGHT.
const MAX_PLAIN = 4;
const MAX_WITH_MEDIA = 3;
const SHOW_MS = 6000;

let nextId = 1;

export default function NotifyPopup() {
  const [cards, setCards] = useState<Card[]>([]);
  const timers = useRef<Map<number, number>>(new Map());
  const stackRef = useRef<HTMLDivElement>(null);

  function remove(id: number) {
    const t = timers.current.get(id);
    if (t !== undefined) window.clearTimeout(t);
    timers.current.delete(id);
    setCards((cs) => cs.filter((c) => c.id !== id));
  }

  function arm(id: number) {
    const old = timers.current.get(id);
    if (old !== undefined) window.clearTimeout(old);
    timers.current.set(id, window.setTimeout(() => remove(id), SHOW_MS));
  }

  // Receive pushes. A fresh theme fetch per push keeps the pop-up in step with
  // a theme change made while the app is running.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    listen<{ title: string; body: string; media_url?: string | null }>("notify:push", async (e) => {
      try {
        const { id: themeId, css } = await api.getActiveTheme();
        const style = document.getElementById("active-theme");
        if (style && document.documentElement.dataset.theme !== themeId) {
          style.textContent = css;
          document.documentElement.dataset.theme = themeId;
        }
      } catch { /* keep the current look */ }
      const card: Card = { id: nextId++, title: e.payload.title, body: e.payload.body, mediaUrl: e.payload.media_url ?? null };
      setCards((cs) => {
        const next = [...cs, card];
        // Drop the oldest beyond the cap so the window never outgrows itself.
        const limit = next.some((x) => x.mediaUrl) ? MAX_WITH_MEDIA : MAX_PLAIN;
        while (next.length > limit) {
          const dropped = next.shift()!;
          const t = timers.current.get(dropped.id);
          if (t !== undefined) window.clearTimeout(t);
          timers.current.delete(dropped.id);
        }
        return next;
      });
      arm(card.id);
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
      timers.current.forEach((t) => window.clearTimeout(t));
      timers.current.clear();
    };
  }, []);

  // Size/show/hide the native window to fit what is on screen. The HEIGHT is
  // measured, not computed: a card with a video in it is taller than one without,
  // and the video's own size is only known once it has loaded.
  useEffect(() => {
    if (cards.length === 0) { api.notifyLayout(0, 0).catch(() => {}); return; }
    const el = stackRef.current;
    const report = () => api.notifyLayout(cards.length, el ? el.offsetHeight : 0).catch(() => {});
    report();
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(report);
    ro.observe(el);
    return () => ro.disconnect();
  }, [cards.length]);

  return (
    <div className="notify-popup-stack" ref={stackRef}>
      {cards.map((c) => (
        <div
          key={c.id}
          className="notify-popup-card"
          role="status"
          onMouseEnter={() => {
            const t = timers.current.get(c.id);
            if (t !== undefined) window.clearTimeout(t); // reading it: hold it
          }}
          onMouseLeave={() => arm(c.id)}
          onClick={() => {
            api.notifyActivate().catch(() => {});
            remove(c.id);
          }}
        >
          <div className="notify-popup-row">
            <div className="notify-popup-text">
              <div className="notify-popup-title">{c.title}</div>
              <div className="notify-popup-body">{c.body}</div>
            </div>
            <button
              className="notify-popup-close"
              aria-label="Dismiss"
              onClick={(e) => {
                e.stopPropagation();
                remove(c.id);
              }}
            >
              ×
            </button>
          </div>
          {c.mediaUrl && <PopupMedia url={c.mediaUrl} />}
        </div>
      ))}
    </div>
  );
}

/**
 * A muted, looping preview of a direct video/image link. Only ever mounted for a
 * sender the person trusts (decided before the notification is raised). It goes
 * through the same relay-proxied path as media in chat, so the link's host never
 * sees the viewer's address, and it renders nothing at all if anything fails.
 */
function PopupMedia({ url }: { url: string }) {
  const state = useLinkEmbed(url, true);
  const media = state.status === "ok" && state.embed.media?.playable_inline ? state.embed.media : null;
  const isVideo = !!media?.mime.startsWith("video/");
  const blob = useProxiedMedia(media?.url ?? null, !!media);
  if (!media || !blob) return null;
  return isVideo
    ? <video className="notify-popup-media" src={blob} autoPlay muted loop playsInline />
    : <img className="notify-popup-media" src={blob} alt="" />;
}
