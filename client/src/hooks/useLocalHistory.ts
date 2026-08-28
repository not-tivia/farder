import { useEffect } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

// ---------------------------------------------------------------------------
// Local history hydration (sub-7a T10).
//
// An E2EE channel's history lives ONLY on this device: opening a sealed message
// consumes that generation's ratchet key in the persisted MLS store, so the same
// ciphertext can never be opened twice. Before this hook existed, a restart
// re-fetched the ciphertext, failed to open it, and rendered every message the
// user had already read as "couldn't decrypt".
//
// This hook loads what we stored on the first read and feeds it into
// `sealedDecrypts` — the same cache a live decrypt writes to — so the rest of
// the UI needs no special case. Two properties matter:
//
//  1. It runs BEFORE any decrypt for that channel. `useSealedDecrypt` is gated on
//     `historyHydrated[channelId]`, because opening a message we already hold
//     would fail (the key is gone) and cache that failure over good history.
//  2. It never opens ciphertext itself. Rendering from the local store is also
//     what stops a restart from burning one ratchet key per message for nothing.
// ---------------------------------------------------------------------------

const inFlight = new Set<string>();

/** How much stored history to hydrate per channel. Matches the history page size
 *  the chat view asks the server for, so the local and remote pages line up. */
const HYDRATE_LIMIT = 200;

export function useLocalHistory(): void {
  const { state, dispatch } = useApp();
  const activeServerId = state.activeServerId;
  const server = activeServerId ? state.servers[activeServerId] : undefined;
  const messages = server?.messages ?? {};
  const historyHydrated = server?.historyHydrated ?? {};

  useEffect(() => {
    if (!activeServerId) return;

    for (const channelIdStr of Object.keys(messages)) {
      const channelId = Number(channelIdStr);
      if (historyHydrated[channelId]) continue;

      const key = `${activeServerId}:${channelId}`;
      if (inFlight.has(key)) continue;
      inFlight.add(key);

      api
        .historyPage(channelId, null, HYDRATE_LIMIT)
        .then((rows) => {
          for (const row of rows) {
            dispatch({
              type: "SEALED_DECRYPTED",
              serverId: activeServerId,
              payload: {
                messageId: row.message_id,
                // Restore null-ness. `historyPut` stores a missing event hash as
                // "", and `useSealedDecrypt` compares the cached `eventHash`
                // against the message row's (`string | null`) to decide whether a
                // row still needs opening. Handing back "" where the row has null
                // would look like a MISMATCH, re-open a message we already hold,
                // fail (the key is consumed) and cache "couldn't decrypt" over
                // the history we just restored.
                eventHash: row.event_hash === "" ? null : row.event_hash,
                content: row.content,
              },
            });
          }
        })
        .catch((err) => {
          // A locked identity or a missing store is not an error state for the
          // user: nothing is hydrated, and `useSealedDecrypt` proceeds exactly as
          // it did before this hook existed. Hydration must never block reading.
          console.warn("[history] hydrate failed:", err);
        })
        .finally(() => {
          inFlight.delete(key);
          // Mark hydrated even on failure, or the decrypt gate would keep the
          // channel closed forever — the deadlock shape this codebase keeps
          // rediscovering (an over-conservative guard with no exit).
          dispatch({ type: "HISTORY_HYDRATED", serverId: activeServerId, payload: { channelId } });
        });
    }
  }, [activeServerId, messages, historyHydrated, dispatch]);
}
