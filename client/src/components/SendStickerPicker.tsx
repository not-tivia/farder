import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { useBookItemSrc } from "./BookItemTile";

interface Props {
  serverId: string;
  channelId: number;
  onClose: () => void;
}

const popover: CSSProperties = {
  position: "absolute",
  bottom: "calc(100% + 4px)",
  left: 0,
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 8,
  width: 320,
  maxHeight: 380,
  zIndex: 1100,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
  display: "flex",
  flexDirection: "column",
  gap: 6,
};

function StickerTile({
  item,
  onSend,
}: {
  item: BookItem;
  onSend: (item: BookItem) => void;
}) {
  const src = useBookItemSrc(item.id);
  return (
    <button
      onClick={() => onSend(item)}
      title={`:${item.name}:`}
      style={{
        width: 64,
        height: 64,
        padding: 0,
        background: "transparent",
        border: "1px solid transparent",
        cursor: "pointer",
      }}
    >
      <img
        src={src}
        alt={item.name}
        style={{ width: "100%", height: "100%", objectFit: "contain" }}
      />
    </button>
  );
}

export default function SendStickerPicker({ serverId, channelId, onClose }: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [items, setItems] = useState<BookItem[]>([]);
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    bookApi.bookListItems().then(setItems).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    function handleMouse(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", handleMouse);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleMouse);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  const visible = useMemo(() => {
    let out = [...items].sort((a, b) => b.added_at - a.added_at);
    if (search.trim()) {
      const q = search.toLowerCase();
      out = out.filter((i) => i.name.toLowerCase().includes(q));
    }
    return out;
  }, [items, search]);

  async function send(item: BookItem) {
    try {
      const fileId = await bookApi.bookGetFileForServer(serverId, item.id);
      await api.sendMessage(serverId, channelId, "", undefined, [fileId]);
      onClose();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div ref={ref} style={popover}>
      <input
        autoFocus
        placeholder="Search…"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ font: "inherit", padding: "2px 6px" }}
      />
      {error && <div style={{ color: "#a00", fontSize: 10 }}>{error}</div>}
      <div style={{ overflowY: "auto", display: "flex", flexWrap: "wrap", gap: 4 }}>
        {visible.length === 0 && !error && (
          <div style={{ padding: 16, color: "var(--xp-text-muted, #666)", textAlign: "center", width: "100%" }}>
            {items.length === 0
              ? "Your book is empty. Open the 📚 button to add items."
              : "No items match your search."}
          </div>
        )}
        {visible.map((item) => (
          <StickerTile key={item.id} item={item} onSend={send} />
        ))}
      </div>
    </div>
  );
}
