import { useEffect, useState, type CSSProperties } from "react";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { resolveBookItemSrc } from "./BookItemTile";

const COMMON_EMOJI = [
  "👍", "👎", "❤️", "😂", "😮", "😢", "😡", "🎉",
  "🔥", "✅", "❌", "🙏", "👀", "💯", "🤔", "⭐",
];

interface ReactionPickerProps {
  onSelect: (emoji: string) => void;
  onSelectBookItem?: (item: BookItem) => void;
  onOpenBookBrowser?: () => void;
}

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
}: ReactionPickerProps) {
  const [bookItems, setBookItems] = useState<BookItem[]>([]);

  useEffect(() => {
    bookApi
      .bookListItems()
      .then((items) =>
        setBookItems([...items].sort((a, b) => b.added_at - a.added_at).slice(0, 12)),
      )
      .catch(() => {});
  }, []);

  return (
    <div className="reaction-picker">
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
            {emoji}
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
}
