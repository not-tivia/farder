import { useState, type CSSProperties } from "react";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { resolveBookItemSrc } from "./BookItemTile";

interface Props {
  item: BookItem;
  onClose: () => void;
  onChanged: () => void;
}

const popover: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.4)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2200,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 16,
  width: 360,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function BookItemDetail({ item, onClose, onChanged }: Props) {
  const [name, setName] = useState(item.name);
  const [error, setError] = useState<string | null>(null);

  async function save() {
    if (name === item.name) {
      onClose();
      return;
    }
    try {
      await bookApi.bookRenameItem(item.id, name);
      onChanged();
      onClose();
    } catch (e) {
      setError(String(e));
    }
  }

  async function del() {
    if (!window.confirm(`Delete "${item.name}"? This removes the file from disk and can't be undone.`)) return;
    try {
      await bookApi.bookDeleteItem(item.id);
      onChanged();
      onClose();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div style={popover} onClick={onClose}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <div style={{ display: "flex", gap: 16 }}>
          <img
            src={resolveBookItemSrc(item)}
            alt={item.name}
            style={{ width: 96, height: 96, objectFit: "contain", border: "1px solid var(--xp-border, #888)" }}
          />
          <div style={{ flex: 1 }}>
            <label style={{ fontSize: 11, display: "block", marginBottom: 4 }}>Name</label>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              style={{ width: "100%", font: "inherit" }}
            />
            <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 8 }}>
              {item.width}×{item.height}px · {item.animated ? "animated" : "static"} · {item.ext.toUpperCase()}
            </div>
            <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>
              source: {item.source}
            </div>
          </div>
        </div>
        {error && <div style={{ color: "#a00", marginTop: 8, fontSize: 11 }}>{error}</div>}
        <div style={{ display: "flex", justifyContent: "space-between", marginTop: 16 }}>
          <button
            onClick={del}
            style={{
              font: "inherit",
              color: "#a00",
              background: "transparent",
              border: "1px solid #a00",
              padding: "4px 12px",
              cursor: "pointer",
            }}
          >
            Delete
          </button>
          <div style={{ display: "flex", gap: 6 }}>
            <button onClick={onClose} style={{ font: "inherit", padding: "4px 12px" }}>
              Cancel
            </button>
            <button
              onClick={save}
              style={{
                font: "inherit",
                padding: "4px 12px",
                background: "var(--xp-blue, #0058E6)",
                color: "#fff",
                border: "1px solid var(--xp-blue-dark, #003C74)",
              }}
            >
              Save
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
