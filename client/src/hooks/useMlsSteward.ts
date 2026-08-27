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
export function useMlsSteward(): void {
  const { state } = useApp();
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
    api.processMlsControlEvents(activeServerId, logServerId, channelId).catch(() => {});
  }, [activeServerId, channelId, logServerId, channel]);
}
