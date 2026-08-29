import { useEffect, useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { publicKeyToString } from "../lib/types";

// ---------------------------------------------------------------------------
// Decrypt-once guard (D2/D4): the ratchet is consumed on open, so a sealed
// ciphertext can be handed to `decrypt_sealed_message` EXACTLY once. Two layers
// enforce that:
//
//  1. Terminal cache — `PerServerState.sealedDecrypts[messageId]` holds the
//     result once a decrypt has settled (decrypted or undecryptable). A row
//     whose (messageId, eventHash) pair is already cached is never re-opened.
//  2. In-flight dedupe — this module-level map keys `${serverId}:${messageId}:
//     ${eventHash}` to the single outstanding `invoke` promise. A re-render (or
//     React StrictMode's double-mount) that sees the same sealed row again joins
//     the existing promise instead of issuing a second `invoke`. `eventHash` is
//     part of the key so a sealed edit (same id, new ciphertext/event hash)
//     arriving mid-decrypt issues a fresh open rather than re-using the stale
//     in-flight result.
//
// Even if a caller kept a byte-for-byte clone of the ciphertext and forced a
// second open, the backend is still structurally once: `receive_sealed` takes
// the ciphertext BY VALUE and the generation's decryption key is already
// consumed, so the second attempt deterministically returns `undecryptable`.
const inFlight = new Map<string, Promise<void>>();

// Our own public key.
//
// This hook mounts at app startup, BEFORE the PIN is entered, and while the
// identity is locked `get_public_key` correctly answers "nothing". Caching that
// `null` as the final answer left the app not knowing who it was for the whole
// session — which is exactly how an author's own message ended up being handed
// to the decryptor and cached as "couldn't decrypt".
//
// So: only a REAL key is cached. A `null` clears the in-flight promise so the
// next caller asks again, once unlocking has actually happened. Handling only
// the throwing case (as the previous version did) misses this, because "locked"
// is a successful answer, not an error.
let cachedOwnPk: string | null = null;
let ownPkPromise: Promise<string | null> | null = null;
function getOwnPk(): Promise<string | null> {
  if (cachedOwnPk != null) return Promise.resolve(cachedOwnPk);
  if (!ownPkPromise) {
    ownPkPromise = api
      .getPublicKey()
      .then((pk) => {
        if (pk) cachedOwnPk = pk;
        else ownPkPromise = null; // still locked — ask again next time
        return pk;
      })
      .catch(() => { ownPkPromise = null; return null; });
  }
  return ownPkPromise;
}

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
  const historyHydrated = server?.historyHydrated ?? {};
  const ownSealedSends = server?.ownSealedSends ?? {};
  const [ownPk, setOwnPk] = useState<string | null>(cachedOwnPk);

  useEffect(() => {
    if (!activeServerId || !logServerId) return;

    // Fail SAFE on not knowing who we are. Every decision below depends on
    // telling our own messages apart from everyone else's, and getting that
    // wrong is destructive: a sealed row can only be opened once, so decrypting
    // one of ours burns its key and caches a permanent failure over the author's
    // own words. "Don't know yet" must therefore mean "do nothing yet", never
    // "assume it isn't mine". The fetch below resolves within a tick of unlock,
    // and this effect re-runs when it lands.
    if (!ownPk) {
      void getOwnPk().then((pk) => { if (pk) setOwnPk(pk); });
      return;
    }

    for (const [channelIdStr, list] of Object.entries(messages)) {
      const channelId = Number(channelIdStr);
      // Gate (T10): never open a ciphertext before the local store has been
      // consulted. A message we already hold has had its ratchet key consumed,
      // so the open would fail and cache "couldn't decrypt" over good history.
      if (!historyHydrated[channelId]) continue;
      for (const msg of list) {
        // A sealed row has empty content and ciphertext in `sealed`.
        const isSealed = msg.is_e2ee === true && msg.sealed != null && msg.content === "";
        if (!isSealed) continue;
        const ciphertext = msg.sealed;
        if (!ciphertext || ciphertext.length === 0) continue;

        const eventHash = msg.event_hash ?? null;
        const existing = sealedDecrypts[msg.id];

        // OUR OWN send: render what we typed. A sender cannot decrypt its own
        // MLS message (the ratchet has no decryption side for it), so handing
        // this to `decrypt_sealed_message` shows the author their own words as
        // "couldn't decrypt".
        //
        // This check MUST come before the settled-cache guard below. The server
        // broadcasts the sealed row back to us before `send_sealed_message`
        // returns, so the decrypt pass races ahead of the event hash we are
        // waiting on, fails, and caches the failure. If the guard ran first, a
        // settled failure would be permanent and the echo could never correct
        // it — which is exactly the bug this ordering fixes. Overwriting a
        // cached failure with the author's own text is always right: we are the
        // authority on what we typed.
        // Belt and braces: NEVER hand a row we authored to the decryptor. A
        // sender has no decryption side for its own message, so the attempt is
        // guaranteed to fail and — worse — caches a failure that renders the
        // author's own words as "couldn't decrypt". If our text is missing (the
        // send reported an error after the server had already accepted it, or it
        // was sent from another device) the right answer is to leave the row
        // pending, not to prove the impossible.
        // `ownPk` is guaranteed non-null here (checked above).
        if (publicKeyToString(msg.author) === ownPk && !(eventHash && ownSealedSends[eventHash])) {
          continue;
        }

        const own = eventHash ? ownSealedSends[eventHash] : undefined;
        if (own !== undefined) {
          if (existing?.kind === "decrypted" && existing.eventHash === eventHash) continue;
          dispatch({
            type: "SEALED_DECRYPTED",
            serverId: activeServerId,
            payload: { messageId: msg.id, eventHash, content: own },
          });
          void api
            .historyPut({
              channel_id: channelId,
              message_id: msg.id,
              event_hash: eventHash ?? "",
              timestamp: msg.timestamp ?? 0,
              author: msg.author?.bytes ?? [],
              content: own,
              reply_to: null,
              attachments: [],
            })
            .catch((e) => console.warn("[history] put (own send) failed:", e));
          continue;
        }

        // Already settled for THIS ciphertext (same message id + event hash) —
        // never re-open it. The ratchet is consumed on open, so a retry cannot
        // succeed anyway.
        if (existing && existing.eventHash === eventHash) continue;

        const key = `${activeServerId}:${msg.id}:${eventHash}`;
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
              // Persist it (T9): this is the ONLY writer, and it runs only on a
              // SUCCESSFUL decrypt — a failure must never be cached as history.
              // The key was just consumed, so if this write is lost the message
              // is unrecoverable; it is still fire-and-forget, because failing
              // the render over a storage error would help nobody.
              void api
                .historyPut({
                  channel_id: channelId,
                  message_id: msg.id,
                  event_hash: eventHash ?? "",
                  timestamp: msg.timestamp ?? 0,
                  author: msg.author?.bytes ?? [],
                  content: result.envelope.content,
                  reply_to: null,
                  attachments: [],
                })
                .catch((e) => console.warn("[history] put failed:", e));
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
  }, [activeServerId, logServerId, messages, sealedDecrypts, historyHydrated, ownSealedSends, ownPk, dispatch]);
}
