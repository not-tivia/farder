import { useEffect, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { useClickAnchoredPosition } from "../lib/useClickAnchoredPosition";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { resolveBookItemSrc } from "./BookItemTile";
import { renderUnicodeEmoji } from "../lib/unicodeEmoji";

const COMMON_EMOJI = [
  "👍", "👎", "❤️", "😂", "😮", "😢", "😡", "🎉",
  "🔥", "✅", "❌", "🙏", "👀", "💯", "🤔", "⭐",
];

interface ReactionPickerProps {
  onSelect: (emoji: string) => void;
  onSelectBookItem?: (item: BookItem) => void;
  onOpenBookBrowser?: () => void;
  /** Where the "+" was clicked. When given, the picker is drawn at the top of the
   *  page next to that point and kept inside the window. Drawn inside the message
   *  instead, it opened upward into the scrolling list, which cut it off (near the
   *  top only the last few emoji could be reached), and the theme's 8-column grid
   *  squeezed the buttons into one tall column. */
  at?: { x: number; y: number };
  /** Called on a click outside the picker or Escape (anchored mode). */
  onClose?: () => void;
}

/** 8 buttons of 28px + gaps + padding: a fixed block, whatever the theme's grid says. */
const PICKER_W = 8 * 30 + 16;

const sectionLabel: CSSProperties = {
  fontSize: 9,
  color: "var(--xp-text-muted, #666)",
  padding: "0 4px 2px",
  fontWeight: "bold",
};

const tileBtn: CSSProperties = {
  width: 28,
  height: 28,
  padding: 0,
  border: "1px solid transparent",
  background: "transparent",
  cursor: "pointer",
  fontSize: 18,
};

export default function ReactionPicker({
  onSelect,
  onSelectBookItem,
  onOpenBookBrowser,
  at,
  onClose,
}: ReactionPickerProps) {
  const [bookItems, setBookItems] = useState<BookItem[]>([]);
  const ref = useRef<HTMLDivElement | null>(null);
  const pos = useClickAnchoredPosition(ref, at ?? { x: 0, y: 0 }, {
    capHeight: true,
    elementW: PICKER_W,
    elementH: ref.current?.offsetHeight ?? 0,
  });
  useEffect(() => {
    if (!at || !onClose) return;
    const onDown = (e: MouseEvent) => { if (ref.current && !ref.current.contains(e.target as Node)) onClose(); };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    // Next tick: the click that opened the picker must not close it.
    const t = setTimeout(() => document.addEventListener("mousedown", onDown), 0);
    window.addEventListener("keydown", onKey);
    return () => { clearTimeout(t); document.removeEventListener("mousedown", onDown); window.removeEventListener("keydown", onKey); };
  }, [at, onClose]);

  useEffect(() => {
    bookApi
      .bookListItems()
      .then((items) =>
        setBookItems([...items].sort((a, b) => b.added_at - a.added_at).slice(0, 12)),
      )
      .catch(() => {});
  }, []);

  const anchoredStyle: CSSProperties | undefined = at
    ? { ...pos, display: "block", width: PICKER_W, boxSizing: "border-box", zIndex: 1000 }
    : undefined;
  const picker = (
    <div className="reaction-picker" ref={ref} style={anchoredStyle}>
      {bookItems.length > 0 && onSelectBookItem && (
        <>
          <div style={sectionLabel}>YOUR BOOK</div>
          <div
            style={{
              display: "flex",
              flexWrap: "wrap",
              gap: 2,
              paddingBottom: 4,
              borderBottom: "1px solid var(--xp-border, #ddd)",
              marginBottom: 4,
            }}
          >
            {bookItems.map((item) => (
              <button
                key={item.id}
                onClick={() => onSelectBookItem(item)}
                title={`:${item.name}:`}
                style={tileBtn}
              >
                <img
                  src={resolveBookItemSrc(item)}
                  alt={item.name}
                  style={{ width: "100%", height: "100%", objectFit: "contain" }}
                />
              </button>
            ))}
            {onOpenBookBrowser && (
              <button
                onClick={onOpenBookBrowser}
                title="Open book"
                style={{ ...tileBtn, fontSize: 14, color: "var(--xp-text-muted, #666)" }}
              >
                +
              </button>
            )}
          </div>
          <div style={sectionLabel}>COMMON</div>
        </>
      )}
      <div style={{ display: "flex", flexWrap: "wrap", gap: 2 }}>
        {COMMON_EMOJI.map((emoji) => (
          <button
            key={emoji}
            className="reaction-picker-btn"
            onClick={() => onSelect(emoji)}
            title={emoji}
            style={tileBtn}
          >
            {renderUnicodeEmoji(emoji)}
          </button>
        ))}
      </div>
      {bookItems.length === 0 && onOpenBookBrowser && (
        <div
          role="button"
          tabIndex={0}
          onClick={onOpenBookBrowser}
          style={{
            marginTop: 6,
            paddingTop: 6,
            borderTop: "1px solid var(--xp-border, #ddd)",
            fontSize: 10,
            color: "var(--xp-blue, #0058E6)",
            cursor: "pointer",
            textDecoration: "underline",
            textAlign: "center",
          }}
        >
          Add custom emoji…
        </div>
      )}
    </div>
  );
  return at ? createPortal(picker, document.body) : picker;
}
