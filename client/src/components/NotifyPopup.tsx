import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../lib/tauri-bridge";

// The view rendered inside the detached, always-on-top "notify" window (see
// src-tauri/src/notify_popup.rs). It shows the ACTIVE THEME's look, holds the
// text only in memory, and fetches nothing: the only input is the local
// `notify:push` event the Rust side emits.

interface Card {
  id: number;
  title: string;
  body: string;
}

const MAX_CARDS = 4; // keep in step with notify_popup::MAX_CARDS
const SHOW_MS = 6000;

let nextId = 1;

export default function NotifyPopup() {
  const [cards, setCards] = useState<Card[]>([]);
  const timers = useRef<Map<number, number>>(new Map());

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
    listen<{ title: string; body: string }>("notify:push", async (e) => {
      try {
        const { id: themeId, css } = await api.getActiveTheme();
        const style = document.getElementById("active-theme");
        if (style && document.documentElement.dataset.theme !== themeId) {
          style.textContent = css;
          document.documentElement.dataset.theme = themeId;
        }
      } catch { /* keep the current look */ }
      const card: Card = { id: nextId++, title: e.payload.title, body: e.payload.body };
      setCards((cs) => {
        const next = [...cs, card];
        // Drop the oldest beyond the cap so the window never outgrows itself.
        while (next.length > MAX_CARDS) {
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

  // Size/show/hide the native window to fit what is on screen.
  useEffect(() => {
    api.notifyLayout(cards.length).catch(() => {});
  }, [cards.length]);

  return (
    <div className="notify-popup-stack">
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
      ))}
    </div>
  );
}
