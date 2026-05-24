// client/src/components/MessageSearchOverlay.tsx
import { useEffect, useMemo, useRef, useState } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import type { MessageInfo } from "../lib/types";
import Message from "./Message";
import { useMessageContext } from "../hooks/useMessageContext";

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
  const [selectedIndex, setSelectedIndex] = useState(0);
  // Debounced "stable selection" — what actually drives the preview fetch.
  // Updated 200ms after selectedIndex stops changing.
  const [stableIndex, setStableIndex] = useState(0);

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

  useEffect(() => {
    if (!open) {
      setQuery("");
      setStatus({ kind: "idle" });
      setSelectedIndex(0);
      setStableIndex(0);
    }
  }, [open]);

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
        setSelectedIndex(0);
        setStableIndex(0);
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

  // Debounce stableIndex 200ms behind selectedIndex.
  useEffect(() => {
    const t = setTimeout(() => setStableIndex(selectedIndex), 200);
    return () => clearTimeout(t);
  }, [selectedIndex]);

  // ↑ / ↓ keyboard nav.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (status.kind !== "ready" || status.results.length === 0) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIndex((i) => (i + 1) % status.results.length);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIndex((i) =>
          (i - 1 + status.results.length) % status.results.length,
        );
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [open, status]);

  const results = status.kind === "ready" ? status.results : [];
  const selectedResult = results[stableIndex] ?? null;

  const context = useMessageContext(
    serverId,
    selectedResult?.channel_id ?? null,
    selectedResult?.id ?? null,
  );

  const channelsById = useMemo(
    () => new Map((activeServer?.channels ?? []).map((c) => [c.id, c])),
    [activeServer?.channels],
  );

  const memberNames: Record<string, string> = useMemo(() => {
    const out: Record<string, string> = {};
    for (const m of activeServer?.members ?? []) {
      out[publicKeyToString(m.public_key)] = m.display_name;
    }
    return out;
  }, [activeServer?.members]);

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
            {status.kind === "ready" && results.length === 0 && (
              <div style={{ color: "var(--text-muted, #888)" }}>
                No messages match "{query.trim()}".
              </div>
            )}
            {results.map((msg, i) => {
              const ch = channelsById.get(msg.channel_id);
              const authorName = memberNames[publicKeyToString(msg.author)] ?? "unknown";
              const isSelected = i === selectedIndex;
              return (
                <div
                  key={msg.id}
                  onMouseEnter={() => setSelectedIndex(i)}
                  style={{
                    padding: 8,
                    marginBottom: 4,
                    borderRadius: 4,
                    cursor: "pointer",
                    background: isSelected ? "var(--accent-faded, rgba(0,88,230,0.12))" : "transparent",
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
            }}
          >
            {!selectedResult && (
              <div style={{ color: "var(--text-muted, #888)" }}>
                Hover a result to preview.
              </div>
            )}
            {selectedResult && context.status === "loading" && (
              <div style={{ color: "var(--text-muted, #888)" }}>Loading context…</div>
            )}
            {selectedResult && context.status === "error" && (
              <div style={{ color: "var(--error, #c44)" }}>
                Couldn't load context — {context.error}.
              </div>
            )}
            {selectedResult && context.status === "ready" && serverId && (
              <div>
                {context.messages.map((m) => (
                  <Message
                    key={m.id}
                    message={m}
                    memberNames={memberNames}
                    grouped={false}
                    serverId={serverId}
                    highlighted={m.id === selectedResult.id}
                    onReply={() => {}}
                  />
                ))}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
