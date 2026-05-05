import { useEffect, useMemo, useState, type CSSProperties } from "react";
import * as bookApi from "../lib/book/client";
import * as api from "../lib/tauri-bridge";
import type { BookItem, BookFilter, BookSort } from "../lib/book/types";
import BookItemTile from "./BookItemTile";
import BookItemDetail from "./BookItemDetail";
import BookIntro from "./BookIntro";

interface Props {
  onClose: () => void;
}

const INTRO_KEY = "bookIntroDismissed";

const tabBtn = (active: boolean): CSSProperties => ({
  font: "inherit",
  padding: "4px 12px",
  background: active ? "var(--xp-blue, #0058E6)" : "var(--xp-panel-bg, #f0ece0)",
  color: active ? "#fff" : "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  borderRadius: 0,
  cursor: "pointer",
});

export default function BookBrowser({ onClose }: Props) {
  const [items, setItems] = useState<BookItem[]>([]);
  const [filter, setFilter] = useState<BookFilter>("all");
  const [sort, setSort] = useState<BookSort>("recent");
  const [search, setSearch] = useState("");
  const [selected, setSelected] = useState<BookItem | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showIntro, setShowIntro] = useState(localStorage.getItem(INTRO_KEY) !== "true");

  async function refresh() {
    try {
      setItems(await bookApi.bookListItems());
    } catch (e) {
      setError(String(e));
    }
  }
  useEffect(() => { refresh(); }, []);

  const visible = useMemo(() => {
    let out = items;
    if (filter === "static") out = out.filter((i) => !i.animated);
    if (filter === "animated") out = out.filter((i) => i.animated);
    if (search.trim()) {
      const q = search.toLowerCase();
      out = out.filter((i) => i.name.toLowerCase().includes(q));
    }
    if (sort === "recent") out = [...out].sort((a, b) => b.added_at - a.added_at);
    if (sort === "alpha") out = [...out].sort((a, b) => a.name.localeCompare(b.name));
    return out;
  }, [items, filter, sort, search]);

  async function handleUpload() {
    try {
      const path = await api.pickFile();
      if (!path) return;
      const lower = path.toLowerCase();
      if (![".png", ".jpg", ".jpeg", ".gif", ".webp"].some((ext) => lower.endsWith(ext))) {
        setError("Please pick a PNG, JPG, GIF, or WebP image.");
        return;
      }
      await bookApi.bookUploadItem(path);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  function dismissIntro() {
    localStorage.setItem(INTRO_KEY, "true");
    setShowIntro(false);
  }

  return (
    <div
      style={{
        position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)",
        display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1500,
      }}
      onClick={onClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: "var(--xp-window-bg, #ECE9D8)",
          color: "var(--xp-text-normal, #000)",
          border: "2px solid var(--xp-blue-dark, #003C74)",
          borderRadius: "6px 6px 0 0",
          width: 760, maxWidth: "94vw", maxHeight: "88vh",
          display: "flex", flexDirection: "column",
          fontFamily: "var(--xp-font, Tahoma, sans-serif)",
          fontSize: "var(--xp-font-size, 11px)",
          boxShadow: "3px 3px 16px rgba(0,0,0,0.45)", overflow: "hidden",
        }}
      >
        {/* Title */}
        <div
          style={{
            background: "linear-gradient(to bottom, var(--xp-blue, #0058E6), var(--xp-blue-light, #3389FF))",
            color: "#fff", padding: "4px 8px 4px 12px", fontWeight: "bold",
            display: "flex", justifyContent: "space-between",
          }}
        >
          <span>Reaction Book</span>
          <button
            onClick={onClose}
            style={{
              font: "inherit", color: "#fff", background: "transparent",
              border: "1px solid #fff", padding: "0 6px", cursor: "pointer",
            }}
          >
            ✕
          </button>
        </div>

        {/* Tabs + toolbar */}
        <div
          style={{
            padding: 8, display: "flex", gap: 8, alignItems: "center",
            borderBottom: "1px solid var(--xp-border, #888)",
          }}
        >
          <button onClick={() => setFilter("all")} style={tabBtn(filter === "all")}>All</button>
          <button onClick={() => setFilter("static")} style={tabBtn(filter === "static")}>Static</button>
          <button onClick={() => setFilter("animated")} style={tabBtn(filter === "animated")}>Animated</button>
          <input
            placeholder="Search…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            style={{ flex: 1, font: "inherit", padding: "2px 6px" }}
          />
          <select value={sort} onChange={(e) => setSort(e.target.value as BookSort)} style={{ font: "inherit" }}>
            <option value="recent">Recent</option>
            <option value="alpha">A–Z</option>
          </select>
          <button
            onClick={handleUpload}
            style={{
              font: "inherit", padding: "4px 12px",
              background: "var(--xp-blue, #0058E6)", color: "#fff",
              border: "1px solid var(--xp-blue-dark, #003C74)",
            }}
          >
            Upload…
          </button>
        </div>

        {/* Body */}
        <div style={{ padding: 12, overflowY: "auto", flex: 1 }}>
          {error && <div style={{ color: "#a00", marginBottom: 8 }}>{error}</div>}
          {visible.length === 0 && !error && (
            <div style={{ textAlign: "center", padding: 32, color: "var(--xp-text-muted, #666)" }}>
              {items.length === 0 ? "Your book is empty. Upload an image to start." : "No items match your filter."}
            </div>
          )}
          <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
            {visible.map((item) => (
              <BookItemTile key={item.id} item={item} onClick={() => setSelected(item)} />
            ))}
          </div>
        </div>

        {/* Footer */}
        <div style={{ padding: 8, borderTop: "1px solid var(--xp-border, #888)", fontSize: 10, color: "var(--xp-text-muted, #666)" }}>
          {items.length} item{items.length === 1 ? "" : "s"} in your book
        </div>
      </div>
      {selected && <BookItemDetail item={selected} onClose={() => setSelected(null)} onChanged={refresh} />}
      {showIntro && <BookIntro onDismiss={dismissIntro} />}
    </div>
  );
}
