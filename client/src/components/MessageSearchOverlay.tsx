// client/src/components/MessageSearchOverlay.tsx
import { useEffect, useRef, useState } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import type { MessageInfo } from "../lib/types";

interface Props {
  open: boolean;
  onClose: () => void;
}

type SearchStatus =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready"; results: MessageInfo[] }
  | { kind: "error"; reason: string };

export function MessageSearchOverlay({ open, onClose }: Props) {
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const inputRef = useRef<HTMLInputElement>(null);

  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<SearchStatus>({ kind: "idle" });

  useEffect(() => {
    if (!open) return;
    const prevFocus = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => {
      prevFocus?.focus?.();
    };
  }, [open]);

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

  // Reset state when overlay closes — fresh start next time.
  useEffect(() => {
    if (!open) {
      setQuery("");
      setStatus({ kind: "idle" });
    }
  }, [open]);

  // Debounced search: 300ms after last keystroke.
  useEffect(() => {
    if (!open || !serverId) return;
    const trimmed = query.trim();
    if (trimmed.length === 0) {
      setStatus({ kind: "idle" });
      return;
    }
    setStatus({ kind: "loading" });
    let cancelled = false;
    const timer = setTimeout(async () => {
      try {
        const results = await api.searchMessages(serverId, trimmed, undefined, 50);
        if (cancelled) return;
        setStatus({ kind: "ready", results });
      } catch (e) {
        if (cancelled) return;
        setStatus({
          kind: "error",
          reason: e instanceof Error ? e.message : String(e),
        });
      }
    }, 300);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [query, open, serverId]);

  const channelsById = new Map(
    (activeServer?.channels ?? []).map((c) => [c.id, c]),
  );
  const memberNames: Record<string, string> = {};
  for (const m of activeServer?.members ?? []) {
    memberNames[publicKeyToString(m.public_key)] = m.display_name;
  }

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
            value={query}
            onChange={(e) => setQuery(e.target.value)}
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
            }}
          >
            {status.kind === "idle" && (
              <div style={{ color: "var(--text-muted, #888)" }}>Type to search messages.</div>
            )}
            {status.kind === "loading" && (
              <div style={{ color: "var(--text-muted, #888)" }}>Searching…</div>
            )}
            {status.kind === "error" && (
              <div style={{ color: "var(--error, #c44)" }}>
                Search failed — {status.reason}.
              </div>
            )}
            {status.kind === "ready" && status.results.length === 0 && (
              <div style={{ color: "var(--text-muted, #888)" }}>
                No messages match "{query.trim()}".
              </div>
            )}
            {status.kind === "ready" && status.results.map((msg) => {
              const ch = channelsById.get(msg.channel_id);
              const authorName = memberNames[publicKeyToString(msg.author)] ?? "unknown";
              return (
                <div
                  key={msg.id}
                  style={{
                    padding: 8,
                    marginBottom: 4,
                    borderRadius: 4,
                    cursor: "pointer",
                    background: "transparent",
                  }}
                >
                  <div style={{ fontWeight: 600 }}>{authorName}</div>
                  <div style={{ color: "var(--text-muted, #888)", fontSize: 11 }}>
                    in #{ch?.name ?? "unknown"}
                  </div>
                  <div
                    style={{
                      marginTop: 2,
                      whiteSpace: "nowrap",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                    }}
                  >
                    {msg.content}
                  </div>
                </div>
              );
            })}
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
