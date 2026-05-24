# Message Search v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the existing `?`-toggle inline search in `ChatPanel.tsx` with a discoverable magnifying-glass button + Ctrl+K-triggered centered overlay. Hover/arrow-key results to preview message in context; click/Enter to jump to it in its channel with a brief highlight.

**Architecture:** Pure UI — no backend or protocol changes. Reuses existing `searchMessages` and `fetchHistory` Tauri commands. New overlay component is mounted in `AppShell`; open state lives there and is toggled by both a button in `ChatPanel`'s channel header and a global keyboard listener. After-jump highlight uses a new `highlightMessageId` field in `ServerContext` + `HIGHLIGHT_MESSAGE` reducer action, with `ChatPanel` doing the scroll-into-view and `Message` rendering the flash class.

**Tech Stack:** React 18, TypeScript 5.5, Vite 5, Tauri 2 (existing). No new npm deps.

**Spec:** `docs/superpowers/specs/2026-05-23-message-search-design.md`

---

## File structure

**Created:**
- `client/src/components/MessageSearchOverlay.tsx` — modal + dual-pane UI
- `client/src/hooks/useMessageContext.ts` — fetch + per-session cache for context windows around a `(channelId, messageId)`

**Modified:**
- `client/src/components/AppShell.tsx` — own `searchOpen` state; register global `Ctrl+K` / `Cmd+K` listener; mount the overlay; thread `setSearchOpen` down so ChatPanel's button can also open it
- `client/src/components/ChatPanel.tsx` — remove inline-search local state + JSX (lines around 19–22, 60–63, 66–74, 138–163, 177–190 per current file); replace `?` button with magnifying-glass that calls `setSearchOpen(true)`; add `useEffect` that scrolls highlighted message into view and clears the highlight after `ttlMs`
- `client/src/context/ServerContext.tsx` — add `highlightMessageId: number | null` to `PerServerState`; add `HIGHLIGHT_MESSAGE` reducer action
- `client/src/components/Message.tsx` — add stable `id="msg-${message.id}"` attribute on the outer wrapper; accept new optional `highlighted?: boolean` prop and extend className with `search-highlight` when set
- `client/src/main.tsx` — inject `.search-highlight` keyframes once at bootstrap (Farder has no shared global CSS file; themes own all styling, so the animation lives in a dedicated `<style>` element)

---

## Phase 1: Highlight infrastructure

## Task 1: ServerContext — highlightMessageId state + reducer action

**Files:**
- Modify: `client/src/context/ServerContext.tsx`

- [ ] **Step 1: Add `highlightMessageId` to PerServerState interface**

Find (around line 22):
```ts
  voiceStates: Record<number, { publicKey: string; displayName: string }[]>;
  currentVoiceChannelId: number | null;
  ownerPublicKey: string | null;
}
```

Replace with:
```ts
  voiceStates: Record<number, { publicKey: string; displayName: string }[]>;
  currentVoiceChannelId: number | null;
  ownerPublicKey: string | null;
  highlightMessageId: number | null;
}
```

- [ ] **Step 2: Add `highlightMessageId: null` to `initialPerServerState`**

Find the `initialPerServerState` block (around line 36); after `ownerPublicKey: null,` add:
```ts
  highlightMessageId: null,
```

- [ ] **Step 3: Add the HIGHLIGHT_MESSAGE action type to the union**

Locate the per-server action union (search the file for `type: "SELECT_CHANNEL"` — it's the discriminated union of all reducer actions). Add a new variant:
```ts
  | { type: "HIGHLIGHT_MESSAGE"; serverId: string; payload: { messageId: number | null } }
```

Place it alongside the other actions in the union (order doesn't matter).

- [ ] **Step 4: Add the reducer case**

In the per-server reducer function (search for `case "SELECT_CHANNEL":`), add a new case alongside the others:
```ts
    case "HIGHLIGHT_MESSAGE":
      return { ...state, highlightMessageId: action.payload.messageId };
```

- [ ] **Step 5: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 6: Commit**

```
git -C /home/deez/farder add client/src/context/ServerContext.tsx
git -C /home/deez/farder commit -m "feat(client): ServerContext highlightMessageId + HIGHLIGHT_MESSAGE action"
```

(Plus the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.)

---

## Task 2: Message.tsx — stable DOM id + highlight class

**Files:**
- Modify: `client/src/components/Message.tsx`

- [ ] **Step 1: Add `highlighted?: boolean` prop**

Locate the Message component's Props interface (the file's existing pattern uses `interface Props { ... }` or similar; search for the prop list including `message: MessageInfo`). Add:
```ts
  highlighted?: boolean;
```

Then add `highlighted` to the destructuring in the component signature.

- [ ] **Step 2: Add `id` + extend `className` on the outer wrapper**

Find (around line 237-238):
```tsx
  return (
    <div
      className={`message${grouped ? " grouped" : ""}`}
```

Replace with:
```tsx
  return (
    <div
      id={`msg-${message.id}`}
      className={`message${grouped ? " grouped" : ""}${highlighted ? " search-highlight" : ""}`}
```

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/Message.tsx
git -C /home/deez/farder commit -m "feat(client): Message renders stable id + optional search-highlight class"
```

---

## Task 3: main.tsx — inject .search-highlight keyframes globally

**Files:**
- Modify: `client/src/main.tsx`

- [ ] **Step 1: Add a `<style>` injection alongside the theme inject**

In `bootstrap()`, after the existing `document.head.appendChild(style)` for the theme block but before the `bookMigrateLegacyFavorites` call (or at the very end of `bootstrap`, just before React mounts — the exact placement doesn't matter as long as it runs once on startup), add:

```ts
  // Search-highlight flash. Lives outside theme CSS because it's a
  // theme-independent animation — themes can override .search-highlight
  // to change the color but the keyframes are global.
  const searchHighlight = document.createElement("style");
  searchHighlight.id = "search-highlight-keyframes";
  searchHighlight.textContent = `
    @keyframes farderSearchFlash {
      0%   { background-color: rgba(255, 165, 0, 0.45); }
      100% { background-color: transparent; }
    }
    .message.search-highlight {
      animation: farderSearchFlash 1.2s ease-out;
    }
  `;
  document.head.appendChild(searchHighlight);
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/main.tsx
git -C /home/deez/farder commit -m "feat(client): inject .search-highlight keyframes at bootstrap"
```

---

## Task 4: useMessageContext hook

**Files:**
- Create: `client/src/hooks/useMessageContext.ts`

- [ ] **Step 1: Create the hook**

```ts
// client/src/hooks/useMessageContext.ts
import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import type { MessageInfo } from "../lib/types";

export interface MessageContext {
  status: "idle" | "loading" | "ready" | "error";
  messages: MessageInfo[];  // newest-last; the matched message is the last element when status === "ready"
  error?: string;
}

// Module-level cache keyed by `${channelId}:${messageId}`. Persists for the
// lifetime of the renderer process — search overlays open and close many
// times, and a previously-fetched window is the same across openings.
const cache = new Map<string, MessageInfo[]>();

/**
 * Fetches a window of context messages ending at (and including) `messageId`
 * in `channelId`. v1 returns up to 11 messages: the match plus up to 10
 * immediately preceding messages. v1.5 would extend the server protocol with
 * an `after` fetch to give a true 5-before + 5-after window.
 *
 * The hook handles its own AbortController so a rapid selection sweep
 * doesn't end up rendering a stale fetch result.
 */
export function useMessageContext(
  serverId: string | null,
  channelId: number | null,
  messageId: number | null,
): MessageContext {
  const [state, setState] = useState<MessageContext>({ status: "idle", messages: [] });

  useEffect(() => {
    if (!serverId || channelId === null || messageId === null) {
      setState({ status: "idle", messages: [] });
      return;
    }

    const key = `${channelId}:${messageId}`;
    const cached = cache.get(key);
    if (cached) {
      setState({ status: "ready", messages: cached });
      return;
    }

    setState({ status: "loading", messages: [] });
    let cancelled = false;

    (async () => {
      try {
        // `fetchHistory(serverId, channelId, beforeId, limit)` returns messages
        // STRICTLY before `beforeId`, newest-first. To include the match itself
        // in the window, pass `messageId + 1`. We ask for 11 and get up to
        // (match + 10 before).
        const result = await api.fetchHistory(serverId, channelId, messageId + 1, 11);
        if (cancelled) return;
        // result is newest-first; reverse so the match (newest in the window)
        // ends up at the bottom for natural top-to-bottom reading order.
        const ordered = [...result].reverse();
        cache.set(key, ordered);
        setState({ status: "ready", messages: ordered });
      } catch (e) {
        if (cancelled) return;
        setState({
          status: "error",
          messages: [],
          error: e instanceof Error ? e.message : String(e),
        });
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [serverId, channelId, messageId]);

  return state;
}

/** Test helper / explicit cache reset. Not used by the production UI in v1. */
export function _clearMessageContextCache(): void {
  cache.clear();
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/hooks/useMessageContext.ts
git -C /home/deez/farder commit -m "feat(client): useMessageContext hook (context fetch + session cache)"
```

---

## Phase 2: Overlay component

## Task 5: MessageSearchOverlay scaffold (modal shell + close behaviors)

**Files:**
- Create: `client/src/components/MessageSearchOverlay.tsx`

- [ ] **Step 1: Create the file with the modal scaffold (no search logic yet)**

```tsx
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
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors. (The component isn't mounted yet — that happens in Task 10.)

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/MessageSearchOverlay.tsx
git -C /home/deez/farder commit -m "feat(client): MessageSearchOverlay scaffold (modal shell + close handlers)"
```

---

## Task 6: Overlay — results list + debounced search query

**Files:**
- Modify: `client/src/components/MessageSearchOverlay.tsx`

- [ ] **Step 1: Replace the file with the version that wires the search query**

```tsx
// client/src/components/MessageSearchOverlay.tsx
import { useEffect, useRef, useState } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
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
    memberNames[Array.from(m.public_key.bytes).map((b) => b.toString(16).padStart(2, "0")).join("")] = m.display_name;
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
              const authorKey = Array.from(msg.author.bytes).map((b) => b.toString(16).padStart(2, "0")).join("");
              const authorName = memberNames[authorKey] ?? "unknown";
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
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/MessageSearchOverlay.tsx
git -C /home/deez/farder commit -m "feat(client): MessageSearchOverlay results list + debounced query"
```

---

## Task 7: Overlay — selection + preview pane (uses useMessageContext)

**Files:**
- Modify: `client/src/components/MessageSearchOverlay.tsx`

- [ ] **Step 1: Add selectedIndex state + hover/keyboard nav + preview pane**

Replace the file with this version. The only additions are: `selectedIndex` state, hover handlers on result rows, ↑/↓ keyboard navigation, the `useMessageContext` hook call for the selected result, and a real preview pane render.

```tsx
// client/src/components/MessageSearchOverlay.tsx
import { useEffect, useMemo, useRef, useState } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
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
      const key = Array.from(m.public_key.bytes)
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("");
      out[key] = m.display_name;
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
              const authorKey = Array.from(msg.author.bytes)
                .map((b) => b.toString(16).padStart(2, "0"))
                .join("");
              const authorName = memberNames[authorKey] ?? "unknown";
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
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/MessageSearchOverlay.tsx
git -C /home/deez/farder commit -m "feat(client): MessageSearchOverlay selection + preview pane"
```

---

## Task 8: Overlay — click / Enter commits jump

**Files:**
- Modify: `client/src/components/MessageSearchOverlay.tsx`

- [ ] **Step 1: Add the commit handler**

Inside the component, after the existing hooks but before the `if (!open) return null;` line, add:

```tsx
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
```

Add `useApp` to the imports at the top:

```tsx
import { useActiveServer, useActiveServerId, useApp } from "../context/ServerContext";
```

- [ ] **Step 2: Wire up onClick on result rows**

Find the result row `<div key={msg.id} ...>`. Add `onClick={() => commit(msg.id, msg.channel_id)}` alongside the existing `onMouseEnter`.

- [ ] **Step 3: Add Enter-to-commit on the input**

Find the `<input>` element. Add an `onKeyDown` handler:

```tsx
            onKeyDown={(e) => {
              if (e.key === "Enter" && status.kind === "ready" && status.results[selectedIndex]) {
                e.preventDefault();
                const sel = status.results[selectedIndex];
                commit(sel.id, sel.channel_id);
              }
            }}
```

- [ ] **Step 4: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/components/MessageSearchOverlay.tsx
git -C /home/deez/farder commit -m "feat(client): MessageSearchOverlay click/Enter commits jump"
```

---

## Phase 3: Wire into the app

## Task 9: ChatPanel — scroll-into-view + auto-clear effect for highlightMessageId

**Files:**
- Modify: `client/src/components/ChatPanel.tsx`

This task is independent of the overlay — it makes any message that gets a highlight (regardless of source) scroll into view and flash. Wiring the magnifying-glass button + removing the old inline search happens in Task 11.

- [ ] **Step 1: Add the highlightMessageId effect**

Near the top of `ChatPanel()` alongside the other `useEffect` calls, add:

```tsx
  const highlightMessageId = activeServer?.highlightMessageId ?? null;
  useEffect(() => {
    if (highlightMessageId === null) return;
    // Wait one tick so the message is in the DOM if the channel just switched.
    const t = setTimeout(() => {
      const el = document.getElementById(`msg-${highlightMessageId}`);
      el?.scrollIntoView({ block: "center", behavior: "smooth" });
    }, 50);
    // Clear after the flash animation completes (1.2s in CSS).
    const clear = setTimeout(() => {
      if (!serverId) return;
      dispatch({ type: "HIGHLIGHT_MESSAGE", serverId, payload: { messageId: null } });
    }, 1300);
    return () => {
      clearTimeout(t);
      clearTimeout(clear);
    };
  }, [highlightMessageId, serverId, dispatch]);
```

- [ ] **Step 2: Thread `highlighted` down to each `<Message>` render**

Find the existing `channelMessages.map(...)` block (around line 166–174):

```tsx
        {channelMessages.map((msg, i) => {
          const prev = i > 0 ? channelMessages[i - 1] : null;
          const sameAuthor = prev &&
            JSON.stringify(prev.author.bytes) === JSON.stringify(msg.author.bytes);
          const withinWindow = prev &&
            (msg.timestamp - prev.timestamp) < 300;
          const grouped = !!(sameAuthor && withinWindow);
          return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} serverId={serverId} onReply={(msg) => setReplyTo(msg)} />;
        })}
```

Replace the return line with:

```tsx
          return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} serverId={serverId} highlighted={msg.id === highlightMessageId} onReply={(msg) => setReplyTo(msg)} />;
```

Apply the same `highlighted={...}` prop to the second `<Message>` render inside the `searchResults` block (around line 185) — actually, defer this; that block is being removed in Task 11.

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/ChatPanel.tsx
git -C /home/deez/farder commit -m "feat(client): ChatPanel scrolls + flashes highlighted message"
```

---

## Task 10: AppShell — searchOpen state + Ctrl+K listener + mount overlay

**Files:**
- Modify: `client/src/components/AppShell.tsx`

- [ ] **Step 1: Add searchOpen state + global Ctrl+K listener + mount overlay**

At the top of `AppShell()`, add:

```tsx
  const [searchOpen, setSearchOpen] = useState(false);

  // Global Ctrl+K (Cmd+K on macOS) opens / refocuses the search overlay.
  useEffect(() => {
    const isMac = navigator.platform.toUpperCase().startsWith("MAC");
    const handler = (e: KeyboardEvent) => {
      const modPressed = isMac ? e.metaKey : e.ctrlKey;
      if (modPressed && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, []);
```

Add `useState`, `useEffect` to the existing React import if not already present.

At the bottom of the JSX returned by `AppShell` (before the outermost closing tag), add:

```tsx
      <MessageSearchOverlay open={searchOpen} onClose={() => setSearchOpen(false)} />
```

Add the import at the top:

```tsx
import { MessageSearchOverlay } from "./MessageSearchOverlay";
```

- [ ] **Step 2: Expose `setSearchOpen` to ChatPanel**

The cleanest approach is via React context or a prop drill. Since `ChatPanel` is rendered inside `AppShell`, pass `setSearchOpen` down as a prop:

In `AppShell.tsx`, find where `<ChatPanel />` is rendered and update to:

```tsx
<ChatPanel onOpenSearch={() => setSearchOpen(true)} />
```

If `ChatPanel` is rendered via a routing mechanism that doesn't take props, instead expose `setSearchOpen` via a small module-local helper:

```tsx
// At module scope, above export default function AppShell():
let setSearchOpenRef: ((v: boolean) => void) | null = null;
export function openMessageSearch(): void {
  setSearchOpenRef?.(true);
}
```

Then inside `AppShell()`, after `useState`:
```tsx
  useEffect(() => {
    setSearchOpenRef = setSearchOpen;
    return () => { setSearchOpenRef = null; };
  }, []);
```

Use whichever fits the existing `ChatPanel` mount in this file. Read `AppShell.tsx` first; if `<ChatPanel />` is rendered directly with no props, use the prop-drill form; if it's behind any wrapper that strips props, use the module helper.

- [ ] **Step 3: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors. The overlay is mounted but the trigger button hasn't been added yet — that's Task 11.

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add client/src/components/AppShell.tsx
git -C /home/deez/farder commit -m "feat(client): AppShell mounts MessageSearchOverlay + global Ctrl+K"
```

---

## Task 11: ChatPanel — magnifying-glass button + remove old inline search

**Files:**
- Modify: `client/src/components/ChatPanel.tsx`

- [ ] **Step 1: Add the `onOpenSearch` prop (if AppShell uses prop-drill)**

If Task 10 chose the prop-drill form, add the prop. Otherwise import `openMessageSearch` from `./AppShell`.

Prop-drill version — change the component signature:

```tsx
export default function ChatPanel({ onOpenSearch }: { onOpenSearch?: () => void }) {
```

Module-helper version — add an import at the top of `ChatPanel.tsx`:

```tsx
import { openMessageSearch } from "./AppShell";
```

- [ ] **Step 2: Remove the old inline-search state + handler**

Delete these lines from the `ChatPanel()` body (current line numbers as of the spec; verify before deleting):

```tsx
  const [showSearch, setShowSearch] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<MessageInfo[] | null>(null);
  const [searching, setSearching] = useState(false);
```

Inside the `useEffect(() => { ... }, [currentChannelId])` block, delete:

```tsx
    setShowSearch(false);
    setSearchQuery("");
    setSearchResults(null);
```

(Keep `setHasMore(true)` and `setReplyTo(null)`.)

Delete the entire `async function handleSearch() { ... }` block.

Verify the file still compiles after each deletion before moving on — if you broke a downstream reference, fix it before continuing.

- [ ] **Step 3: Replace the `?` button with a magnifying-glass button**

Find (around current line 138-144):

```tsx
        <button
          className="search-toggle"
          onClick={() => setShowSearch(!showSearch)}
          title="Search messages"
        >
          ?
        </button>
```

Replace with:

```tsx
        <button
          className="search-toggle"
          onClick={() => onOpenSearch?.()}
          title="Search messages (Ctrl+K)"
          aria-label="Search messages"
        >
          🔍
        </button>
```

(If Task 10 used the module-helper, replace `onOpenSearch?.()` with `openMessageSearch()`.)

- [ ] **Step 4: Remove the inline `{showSearch && (...)}` search-bar block**

Find and delete the entire block (currently around lines 146–163):

```tsx
      {showSearch && (
        <div className="search-bar">
          <input ... />
          <button className="xp-button" onClick={handleSearch} disabled={searching}>...</button>
          <button className="xp-button" onClick={() => { setShowSearch(false); setSearchResults(null); setSearchQuery(""); }}>X</button>
        </div>
      )}
```

- [ ] **Step 5: Remove the `{searchResults && (...)}` results block**

Find and delete the entire block (currently around lines 177–190):

```tsx
      {searchResults && (
        <div className="search-results">
          <div className="search-results-header">...</div>
          <div className="search-results-list">
            {searchResults.map((msg) => (
              <Message ... />
            ))}
            {searchResults.length === 0 && <div className="search-no-results">No messages found.</div>}
          </div>
        </div>
      )}
```

- [ ] **Step 6: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -10
```

Expected: no errors. If there are unused-import warnings (`MessageInfo` may now be unused if nothing else references it), prune them.

- [ ] **Step 7: Commit**

```
git -C /home/deez/farder add client/src/components/ChatPanel.tsx
git -C /home/deez/farder commit -m "feat(client): replace inline search in ChatPanel with overlay trigger"
```

---

## Phase 4: Smoke + CHANGELOG

## Task 12: Smoke verification + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Run final cargo + tsc checks**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -5
cd /home/deez/farder/client && npx tsc --noEmit 2>&1 | tail -5
```

Expected: `cargo check` finishes (no errors related to this work; pre-existing warnings are fine). `tsc --noEmit` produces no output.

- [ ] **Step 2: Manual smoke (delegate to human)**

The agent cannot drive the Tauri WebView. Report this checklist to the human for them to verify on a non-WSL machine:

```
cd /home/deez/farder/client && npm run tauri dev
```

Checklist:
- [ ] Magnifying-glass button visible in the channel header (replacing the old `?`)
- [ ] Click button → overlay opens, input focused
- [ ] Type a query → results appear within ~1s
- [ ] Hover a different result → preview pane updates (after 200ms)
- [ ] ↑/↓ moves selection; preview follows
- [ ] Click result → overlay closes, channel switches, message scrolls into view, briefly flashes orange (~1.2s)
- [ ] Enter on selection: same as click
- [ ] Esc closes overlay; previous focus restored
- [ ] Click overlay backdrop closes it
- [ ] Ctrl+K (Cmd+K on macOS) opens overlay, works from any focused element
- [ ] Disconnect from server while overlay open → overlay closes cleanly
- [ ] Empty results: shows "No messages match …"
- [ ] No regression: regular chat scroll, history pagination, reactions, replies all still work

- [ ] **Step 3: Add CHANGELOG entry**

In `CHANGELOG.md`, under the `### Added` section (right after the most recent entry — e.g., the translation v1 line), insert:

```markdown
- (2026-05-23) Message search v1: click the magnifying-glass button in the channel header (or press Ctrl+K / Cmd+K) to open a centered search overlay. Type to search the active server's messages (debounced 300ms, up to 50 results, sorted newest-first). Hover or arrow-key any result to preview it in context (the match plus up to 10 messages immediately before — a true "before + after" window is deferred to v1.5 since the server's `fetch_history` only goes backward). Click or Enter on a result closes the overlay, switches to that message's channel, scrolls it into view, and briefly flashes it orange (~1.2s). Replaces the previous `?`-toggle + inline-search-bar UI in `ChatPanel.tsx` (channel-scoped, raw results list, no preview) — the trigger stays in the same channel-header position. UI-only; no backend, protocol, or storage changes. New: `MessageSearchOverlay.tsx`, `useMessageContext.ts` hook. Modified: `ChatPanel.tsx`, `AppShell.tsx`, `ServerContext.tsx` (new `highlightMessageId` field + `HIGHLIGHT_MESSAGE` action), `Message.tsx` (stable DOM id + optional `highlighted` prop), `main.tsx` (injects `.search-highlight` keyframes globally since Farder's CSS is theme-owned).
```

- [ ] **Step 4: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog entry for message search v1"
```

---

## Self-review notes

- Spec coverage: every section of the spec maps to a task. UI/UX → Tasks 5-8. Trigger button + existing-search removal → Task 11. Context infra (DOM id, highlight, scroll) → Tasks 1-3, 9. AppShell wiring → Task 10. CHANGELOG + smoke → Task 12.
- v1 scope compromise (10-before + match, no after) is captured in the `useMessageContext` hook docstring and in the CHANGELOG entry, matching the spec.
- The agent cannot smoke-test the Tauri WebView from WSL; Task 12 explicitly hands the manual checklist back to the human.
- The "prop-drill vs module-helper" choice in Task 10 is left to the implementer because the AppShell + ChatPanel coupling isn't visible from the spec alone. Both options are spelled out.
