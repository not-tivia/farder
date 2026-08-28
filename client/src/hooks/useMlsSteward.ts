import { useEffect, useRef } from "react";
import { useApp } from "../context/ServerContext";
import { isE2eeChannel } from "../lib/types";
import * as api from "../lib/tauri-bridge";

// The steward trigger for "on channel open" (T9, sub-4b): when the user
// selects an E2EE channel, run the receive-side vertical once so any MLS
// control events that arrived while the channel was closed — a Welcome
// addressed to us, or someone else's commit — are applied before the user
// reads the channel. The other half of the trigger (each incoming
// `server:mls_control_event`) lives in useServerEvents.
//
// Non-spinning by construction: the effect fires only when the (server,
// channel) pair changes (tracked in a ref, not a dependency), and the steward
// command itself is cursor-based + idempotent, so a re-run is a cheap no-op.
/** Persist this run's leaf changes as notices, then return the channel's full
 *  notice list for rendering. Best-effort: a storage failure must never break
 *  the steward — a missing notice is a lesser harm than a channel that will not
 *  advance — but it IS logged, because a silently missing transparency notice
 *  defeats the point of having one. */
async function recordLeafNotices(
  serverId: string,
  channelId: number,
  epoch: number,
  gained: api.LeafChange[],
  lost: api.LeafChange[],
): Promise<api.NoticeRow[] | null> {
  try {
    const now = Math.floor(Date.now() / 1000);
    for (const [kind, list] of [["gained", gained], ["lost", lost]] as const) {
      for (const change of list) {
        await api.historyPutNotice({
          channel_id: channelId,
          id: `${kind}:${change.identity}:${change.device}:${epoch}`,
          timestamp: now,
          kind,
          identity: change.identity,
          device: change.device,
        });
      }
    }
    return await api.historyNotices(channelId, 200);
  } catch (e) {
    console.warn(`[e2ee] transparency notice not recorded for ${serverId}/${channelId}:`, e);
    return null;
  }
}

export function useMlsSteward(): void {
  const { state, dispatch } = useApp();
  const activeServerId = state.activeServerId;
  const activeServer = activeServerId ? state.servers[activeServerId] : undefined;
  const channelId = activeServer?.currentChannelId ?? null;
  const logServerId = activeServer?.logServerId ?? null;
  const channel = channelId != null
    ? activeServer?.channels.find((c) => c.id === channelId)
    : undefined;

  const lastRef = useRef<{ serverId: string | null; channelId: number | null }>({
    serverId: null,
    channelId: null,
  });

  useEffect(() => {
    const prev = lastRef.current;
    if (prev.serverId === activeServerId && prev.channelId === channelId) return;
    lastRef.current = { serverId: activeServerId, channelId };

    if (!activeServerId || channelId == null || !logServerId) return;
    if (!isE2eeChannel(channel)) return;

    // Drain on demand; failures (identity still locked, no key package
    // published yet, transport error) are non-fatal and surfaced by the
    // backend log / the T11 UI states, never retried in a loop.
    api.processMlsControlEvents(activeServerId, logServerId, channelId)
      .then((result) => {
        // Surface the steward's verdict into state so T11 can render it
        // (waiting-for-keys / no-history interstitials and the equivocation
        // banner). Rendering T9's result - the steward logic is unchanged.
        dispatch({
          type: "MLS_STATE",
          serverId: activeServerId,
          payload: {
            channelId: result.channel_id,
            confirmed: result.confirmed,
            outcome: result.outcome as ("advanced" | "equivocation"),
            reason: result.reason,
          },
        });
        // G1: turn the leaf diff into in-channel transparency notices. The
        // spec requires a leaf-set change to be visible in the channel — "a new
        // device of Alice can now read #private" — because silent read-access
        // changes are exactly what an attacker wants. Persisted (not toasted)
        // so restarting cannot make you miss one; the id is deterministic, so
        // the cursor-based steward re-observing a change replaces rather than
        // stacks.
        void recordLeafNotices(activeServerId, result.channel_id, result.epoch, result.leaves_gained, result.leaves_lost)
          .then((notices) => {
            if (notices) {
              dispatch({
                type: "SET_NOTICES",
                serverId: activeServerId,
                payload: { channelId: result.channel_id, notices },
              });
            }
          });
        // Publish our KeyPackage only if we are NOT yet a confirmed member.
        // CRITICAL non-destructive guard (T9's flag): `publish_own_key_package`
        // overwrites `mls_state.json` back to `confirmed: false` / epoch 0, so
        // it must never run on an already-confirmed member. `result.confirmed`
        // is the persisted flag, false when `mls_state.json` is absent OR has
        // `confirmed === false` — exactly the publish gate. Already confirmed →
        // skip the publish entirely.
        //
        // `outcome === "equivocation"` is the F4-terminal poisoned state: an
        // impostor leaf could not be bound, so the group is read-frozen and must
        // NEVER be overwritten back to `poisoned: None` / `confirmed: false` by
        // a publish. Skip the publish on equivocation too.
        if (!result.confirmed && result.outcome !== "equivocation") {
          return api.publishOwnKeyPackage(activeServerId, logServerId, channelId);
        }
        return undefined;
      })
      .catch(() => {});
  }, [activeServerId, channelId, logServerId, channel, dispatch]);
}
