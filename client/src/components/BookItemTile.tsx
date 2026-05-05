import { useEffect, useState, type CSSProperties } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";

interface Props {
  item: BookItem;
  onClick: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  selected?: boolean;
}

const tileStyle: CSSProperties = {
  width: 96,
  display: "flex",
  flexDirection: "column",
  alignItems: "center",
  gap: 4,
  padding: 6,
  cursor: "pointer",
  background: "transparent",
  border: "1px solid transparent",
  borderRadius: 4,
  font: "inherit",
  color: "var(--xp-text-normal, #000)",
};

// Resolves an item's absolute on-disk path → Tauri-safe asset URL.
// Returns empty string while loading; the <img> shows broken-image briefly.
export function useBookItemSrc(itemId: string): string {
  const [src, setSrc] = useState("");
  useEffect(() => {
    bookApi
      .bookItemAbsolutePath(itemId)
      .then((p) => setSrc(convertFileSrc(p)))
      .catch(() => setSrc(""));
  }, [itemId]);
  return src;
}

// Backward-compat shim: callers that need a sync src can use this until they
// migrate to useBookItemSrc. It returns the convertFileSrc of the manually-
// constructed path; works on dev's machine but is not portable. Prefer the hook.
export function resolveBookItemSrc(item: { id: string; ext: string }): string {
  return convertFileSrc(`/home/deez/.farder/book/files/${item.id}.${item.ext}`);
}

export default function BookItemTile({ item, onClick, onContextMenu, selected }: Props) {
  const src = useBookItemSrc(item.id);
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onClick}
      onContextMenu={onContextMenu}
      style={{
        ...tileStyle,
        border: selected ? "1px solid var(--xp-blue, #0058E6)" : tileStyle.border,
      }}
    >
      <img
        src={src}
        alt={item.name}
        style={{ width: 64, height: 64, objectFit: "contain", border: "1px solid var(--xp-border, #888)" }}
      />
      <div
        style={{
          fontSize: 10,
          textAlign: "center",
          maxWidth: "100%",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        :{item.name}:
      </div>
    </div>
  );
}
