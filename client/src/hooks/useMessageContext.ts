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
 * The hook handles its own cancellation so a rapid selection sweep doesn't
 * end up rendering a stale fetch result.
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
