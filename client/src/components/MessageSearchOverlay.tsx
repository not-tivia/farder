// client/src/components/MessageSearchOverlay.tsx
import { useEffect, useMemo, useRef, useState } from "react";
import { useActiveServer, useActiveServerId, useApp } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import type { MessageInfo } from "../lib/types";
import Message from "./Message";
import { useMessageContext } from "../hooks/useMessageContext";

interface Props {
  open: boolean;
  /** Bumped each time the overlay should refocus its input — lets Ctrl+K
   *  refocus when the overlay is already open (which `open` going from true
   *  to true is a no-op for React state diffing). */
  openTrigger?: number;
  onClose: () => void;
}

type SearchStatus =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready"; results: MessageInfo[] }
  | { kind: "error"; reason: string };

export function MessageSearchOverlay({ open, openTrigger = 0, onClose }: Props) {
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const inputRef = useRef<HTMLInputElement>(null);

  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<SearchStatus>({ kind: "idle" });
  const [selectedIndex, setSelectedIndex] = useState(0);
  // Debounced "stable selection" — what actually drives the preview fetch.
  // Updated 200ms after selectedIndex stops changing.
  const [stableIndex, setStableIndex] = useState(0);
  // Bumped to retrigger the search effect with the same query (e.g., user hit
  // Enter on an error state to retry).
  const [retryTick, setRetryTick] = useState(0);

  useEffect(() => {
    if (!open) return;
    const prevFocus = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => {
      prevFocus?.focus?.();
    };
  }, [open]);

  // Refocus the input whenever the parent bumps openTrigger (e.g., Ctrl+K
  // fired while the overlay was already open). Selects existing text so a
  // second Ctrl+K behaves like "open fresh search".
  useEffect(() => {
    if (!open || openTrigger === 0) return;
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [openTrigger, open]);

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
  }, [query, open, serverId, retryTick]);

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

  const { dispatch } = useApp();

  function commit(messageId: number, channelId: number) {
    if (!serverId) return;
    onClose();
    dispatch({ type: "SELECT_CHANNEL", serverId, payload: channelId });
    dispatch({
      type: "HIGHLIGHT_MESSAGE",
      serverId,
      payload: { messageId },
    });
  }

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
    // Right-docked side panel. No backdrop, no click-outside-to-close — the
    // user wants the chat to remain interactive while searching. Close via
    // Esc, the × button, or by clicking a result.
    <div
      style={{
        position: "fixed",
        top: 0,
        right: 0,
        bottom: 0,
        width: "min(440px, 90vw)",
        background: "var(--xp-panel-bg, #ECE9D8)",
        color: "inherit",
        borderLeft: "1px solid var(--xp-border, #ACA899)",
        boxShadow: "-4px 0 16px rgba(0, 0, 0, 0.18)",
        display: "flex",
        flexDirection: "column",
        overflow: "hidden",
        zIndex: 1200,
      }}
    >
      <div
        style={{
          flex: 1,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            padding: "12px 16px",
            borderBottom: "1px solid var(--xp-border, #ACA899)",
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
        <div style={{ padding: 12, borderBottom: "1px solid var(--xp-border, #ACA899)" }}>
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              if (status.kind === "ready" && status.results[selectedIndex]) {
                e.preventDefault();
                const sel = status.results[selectedIndex];
                commit(sel.id, sel.channel_id);
              } else if (status.kind === "error") {
                e.preventDefault();
                setRetryTick((n) => n + 1);
              }
            }}
            placeholder="Type to search messages…"
            style={{
              width: "100%",
              padding: "8px 12px",
              fontSize: 14,
              border: "1px solid var(--xp-border, #ACA899)",
              borderRadius: 4,
              background: "var(--xp-window-bg, #ECE9D8)",
              color: "inherit",
            }}
          />
        </div>
        <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden", minHeight: 0 }}>
          <div
            style={{
              flex: selectedResult ? "1 1 50%" : "1 1 auto",
              minHeight: 0,
              overflowY: "auto",
              padding: 8,
              fontSize: 12,
              borderBottom: selectedResult ? "1px solid var(--xp-border, #ACA899)" : "none",
            }}
          >
            {status.kind === "idle" && (
              <div style={{ color: "var(--xp-text-muted, #888880)" }}>Type to search messages.</div>
            )}
            {status.kind === "loading" && (
              <div style={{ color: "var(--xp-text-muted, #888880)" }}>Searching…</div>
            )}
            {status.kind === "error" && (
              <div style={{ color: "var(--error, #c44)" }}>
                Search failed — {status.reason}. Press Enter to retry.
              </div>
            )}
            {status.kind === "ready" && results.length === 0 && (
              <div style={{ color: "var(--xp-text-muted, #888880)" }}>
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
                  onClick={() => commit(msg.id, msg.channel_id)}
                  style={{
                    padding: 8,
                    marginBottom: 4,
                    borderRadius: 4,
                    cursor: "pointer",
                    background: isSelected ? "color-mix(in srgb, var(--xp-blue, #0058E6) 12%, transparent)" : "transparent",
                  }}
                >
                  <div style={{ fontWeight: 600 }}>{authorName}</div>
                  <div style={{ color: "var(--xp-text-muted, #888880)", fontSize: 11 }}>
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
              flex: selectedResult ? "1 1 50%" : "0 0 0",
              minHeight: 0,
              overflowY: "auto",
              padding: selectedResult ? 12 : 0,
              fontSize: 12,
            }}
          >
            {!selectedResult && null /* preview pane collapses when nothing selected */}
            {selectedResult && context.status === "loading" && (
              <div style={{ color: "var(--xp-text-muted, #888880)" }}>Loading context…</div>
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
