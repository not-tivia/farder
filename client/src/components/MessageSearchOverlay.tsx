// client/src/components/MessageSearchOverlay.tsx
import { useEffect, useRef } from "react";
import { useActiveServer } from "../context/ServerContext";

interface Props {
  open: boolean;
  onClose: () => void;
}

export function MessageSearchOverlay({ open, onClose }: Props) {
  const activeServer = useActiveServer();
  const inputRef = useRef<HTMLInputElement>(null);

  // Auto-focus input on open + restore focus on close.
  useEffect(() => {
    if (!open) return;
    const prevFocus = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => {
      prevFocus?.focus?.();
    };
  }, [open]);

  // Esc to close.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0, 0, 0, 0.5)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1200,
      }}
    >
      <div
        style={{
          background: "var(--bg-elevated, #fff)",
          color: "var(--text, #000)",
          width: "min(960px, 90vw)",
          height: "min(640px, 80vh)",
          borderRadius: 8,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
          boxShadow: "0 8px 32px rgba(0, 0, 0, 0.35)",
        }}
      >
        <div
          style={{
            padding: "12px 16px",
            borderBottom: "1px solid var(--border, #ccc)",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <span style={{ fontWeight: 600 }}>
            Search messages in {activeServer?.serverName ?? ""}
          </span>
          <button
            onClick={onClose}
            style={{ background: "none", border: "none", cursor: "pointer", fontSize: 18 }}
            aria-label="Close search"
          >
            ×
          </button>
        </div>
        <div style={{ padding: 12, borderBottom: "1px solid var(--border, #ccc)" }}>
          <input
            ref={inputRef}
            type="text"
            placeholder="Type to search messages…"
            style={{
              width: "100%",
              padding: "8px 12px",
              fontSize: 14,
              border: "1px solid var(--border, #ccc)",
              borderRadius: 4,
              background: "var(--bg, #fff)",
              color: "var(--text, #000)",
            }}
          />
        </div>
        <div style={{ flex: 1, display: "flex", overflow: "hidden" }}>
          <div
            style={{
              width: 320,
              borderRight: "1px solid var(--border, #ccc)",
              overflowY: "auto",
              padding: 8,
              fontSize: 12,
              color: "var(--text-muted, #888)",
            }}
          >
            Type to search messages.
          </div>
          <div
            style={{
              flex: 1,
              overflowY: "auto",
              padding: 12,
              fontSize: 12,
              color: "var(--text-muted, #888)",
            }}
          >
            Hover a result to preview.
          </div>
        </div>
      </div>
    </div>
  );
}
