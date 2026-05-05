import { type CSSProperties } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
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

// FIXME(Task 14): replace with a Rust command (book_item_absolute_path) that
// returns the resolved absolute path. The current hardcoded HOME works on the
// dev machine but breaks for other users.
export function resolveBookItemSrc(item: { id: string; ext: string }): string {
  return convertFileSrc(`/home/deez/.farder/book/files/${item.id}.${item.ext}`);
}

export default function BookItemTile({ item, onClick, onContextMenu, selected }: Props) {
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
        src={resolveBookItemSrc(item)}
        alt={item.name}
        style={{
          width: 64,
          height: 64,
          objectFit: "contain",
          border: "1px solid var(--xp-border, #888)",
        }}
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
