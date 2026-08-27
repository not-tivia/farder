import { useEffect } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

// ---------------------------------------------------------------------------
// Decrypt-once guard (D2/D4): the ratchet is consumed on open, so a sealed
// ciphertext can be handed to `decrypt_sealed_message` EXACTLY once. Two layers
// enforce that:
//
//  1. Terminal cache — `PerServerState.sealedDecrypts[messageId]` holds the
//     result once a decrypt has settled (decrypted or undecryptable). A row
//     whose (messageId, eventHash) pair is already cached is never re-opened.
//  2. In-flight dedupe — this module-level map keys `${serverId}:${messageId}`
//     to the single outstanding `invoke` promise. A re-render (or React
//     StrictMode's double-mount) that sees the same sealed row again joins the
//     existing promise instead of issuing a second `invoke`.
//
// Even if a caller kept a byte-for-byte clone of the ciphertext and forced a
// second open, the backend is still structurally once: `receive_sealed` takes
// the ciphertext BY VALUE and the generation's decryption key is already
// consumed, so the second attempt deterministically returns `undecryptable`.
const inFlight = new Map<string, Promise<void>>();

/**
 * Scan the active server's messages for sealed rows and decrypt each one once.
 *
 * Mounted at the app root next to `useServerEvents` / `useMlsSteward`. It reacts
 * to state, not to any single event, so it covers BOTH arrival paths: a live
 * `server:sealed_message` (folded into `messages` by `useServerEvents`) and a
 * history load (`SET_MESSAGES` / `PREPEND_MESSAGES`). Decrypted content is held
 * in frontend memory only (D4) — it is never persisted to disk in 4b.
 */
export function useSealedDecrypt(): void {
  const { state, dispatch } = useApp();
  const activeServerId = state.activeServerId;
  const server = activeServerId ? state.servers[activeServerId] : undefined;
  const logServerId = server?.logServerId ?? null;
  const messages = server?.messages ?? {};
  const sealedDecrypts = server?.sealedDecrypts ?? {};

  useEffect(() => {
    if (!activeServerId || !logServerId) return;

    for (const [channelIdStr, list] of Object.entries(messages)) {
      const channelId = Number(channelIdStr);
      for (const msg of list) {
        // A sealed row has empty content and ciphertext in `sealed`.
        const isSealed = msg.is_e2ee === true && msg.sealed != null && msg.content === "";
        if (!isSealed) continue;
        const ciphertext = msg.sealed;
        if (!ciphertext || ciphertext.length === 0) continue;

        const eventHash = msg.event_hash ?? null;
        const existing = sealedDecrypts[msg.id];
        // Already decrypted THIS ciphertext (same message id + event hash) —
        // never re-open it.
        if (existing && existing.eventHash === eventHash) continue;

        const key = `${activeServerId}:${msg.id}`;
        if (inFlight.has(key)) continue; // an open is already in flight; join it

        const promise = api
          .decryptSealedMessage(activeServerId, logServerId, channelId, ciphertext)
          .then((result) => {
            if (result.kind === "decrypted") {
              dispatch({
                type: "SEALED_DECRYPTED",
                serverId: activeServerId,
                payload: { messageId: msg.id, eventHash, content: result.envelope.content },
              });
            } else {
              dispatch({
                type: "SEALED_UNDECRYPTABLE",
                serverId: activeServerId,
                payload: { messageId: msg.id, eventHash, reason: result.reason },
              });
            }
          })
          .catch((err) => {
            // A command-level failure (identity locked, store missing, poisoned
            // group) is the same fail-closed state: cache it so the row never
            // retries, and let T11 render the distinct marker.
            dispatch({
              type: "SEALED_UNDECRYPTABLE",
              serverId: activeServerId,
              payload: { messageId: msg.id, eventHash, reason: String(err) },
            });
          })
          .finally(() => {
            inFlight.delete(key);
          });
        inFlight.set(key, promise);
      }
    }
  }, [activeServerId, logServerId, messages, sealedDecrypts, dispatch]);
}
