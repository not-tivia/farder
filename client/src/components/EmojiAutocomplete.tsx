import { useEffect, useState, type CSSProperties } from "react";
import type { BookItem } from "../lib/book/types";
import { searchShorthand, lookupShorthand, renderUnicodeEmoji } from "../lib/unicodeEmoji";
import { useBookItemSrc } from "./BookItemTile";

interface Match {
  kind: "book" | "unicode";
  name: string;
  bookItem?: BookItem;
}

interface Props {
  query: string;
  bookIndex: BookItem[];
  position: { x: number; y: number };
  onSelect: (name: string) => void;
  onClose: () => void;
}

const popover: CSSProperties = {
  position: "fixed",
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 4,
  minWidth: 160,
  zIndex: 1500,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
  transform: "translateY(-100%)",
};

const itemStyle = (selected: boolean): CSSProperties => ({
  display: "flex",
  alignItems: "center",
  gap: 8,
  padding: "4px 8px",
  background: selected ? "var(--xp-blue, #0058E6)" : "transparent",
  color: selected ? "#fff" : "inherit",
  cursor: "pointer",
});

const MAX_RESULTS = 8;

function BookThumb({ item }: { item: BookItem }) {
  const src = useBookItemSrc(item.id);
  return <img src={src} alt={item.name} style={{ width: 18, height: 18, objectFit: "contain" }} />;
}

export default function EmojiAutocomplete({ query, bookIndex, position, onSelect, onClose }: Props) {
  const [selectedIdx, setSelectedIdx] = useState(0);

  const q = query.toLowerCase();
  const bookMatches: Match[] = bookIndex
    .filter((b) => b.name.toLowerCase().startsWith(q))
    .slice(0, MAX_RESULTS)
    .map((b) => ({ kind: "book", name: b.name, bookItem: b }));
  const remaining = MAX_RESULTS - bookMatches.length;
  const unicodeMatches: Match[] =
    remaining > 0
      ? searchShorthand(q, remaining).map((name) => ({ kind: "unicode", name }))
      : [];
  const matches = [...bookMatches, ...unicodeMatches];

  useEffect(() => {
    setSelectedIdx(0);
  }, [query, matches.length]);

  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (matches.length === 0) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIdx((i) => (i + 1) % matches.length);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIdx((i) => (i - 1 + matches.length) % matches.length);
      } else if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        onSelect(matches[selectedIdx].name);
      } else if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    }
    window.addEventListener("keydown", handleKey, true);
    return () => window.removeEventListener("keydown", handleKey, true);
  }, [matches, selectedIdx, onSelect, onClose]);

  if (matches.length === 0) return null;

  return (
    <div style={{ ...popover, top: position.y, left: position.x }}>
      {matches.map((m, i) => (
        <div
          key={`${m.kind}-${m.name}`}
          style={itemStyle(i === selectedIdx)}
          onMouseEnter={() => setSelectedIdx(i)}
          onClick={() => onSelect(m.name)}
        >
          {m.kind === "book" && m.bookItem ? (
            <BookThumb item={m.bookItem} />
          ) : (
            <span style={{ width: 18, textAlign: "center" }}>
              {renderUnicodeEmoji(lookupShorthand(m.name) ?? "")}
            </span>
          )}
          <span>:{m.name}:</span>
        </div>
      ))}
    </div>
  );
}
