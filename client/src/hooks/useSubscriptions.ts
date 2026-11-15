import { useEffect, useRef, useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { WATCHED_CHANGED_EVENT, subscriptionIds } from "../lib/watchedChannels";

/**
 * Tells every connected server which channels we want live messages for.
 *
 * The server only pushes a channel's messages to clients that subscribed to it,
 * and a subscribe call REPLACES the previous list. Subscribing to just the channel
 * on screen meant a message in any other channel (or a DM, or a channel on another
 * server) never arrived until you clicked into it, so unread badges and the buddy
 * list never moved. So: subscribe to every channel the person can see, plus what
 * is on screen and what small chat windows show. The server drops any channel the
 * caller may not see (`subscriptions::apply_subscribe`), so listing them all is safe.
 */
export function useSubscriptions(): void {
  const { state } = useApp();
  const last = useRef(new Map<string, string>());
  const [tick, setTick] = useState(0);

  useEffect(() => {
    const bump = () => setTick((n) => n + 1);
    window.addEventListener(WATCHED_CHANGED_EVENT, bump);
    return () => window.removeEventListener(WATCHED_CHANGED_EVENT, bump);
  }, []);

  useEffect(() => {
    for (const sid of state.serverList.map((s) => s.id)) {
      const sv = state.servers[sid];
      // While the connection is down the server forgot us: forget what we sent,
      // so the list goes out again once we are back.
      if (!sv || !sv.connected || sv.connectionLost) { last.current.delete(sid); continue; }
      const ids = subscriptionIds(
        [
          sv.currentChannelId, sv.dmPanelChannelId, sv.threadChannelId,
          ...sv.channels.filter((c) => c.channel_type !== "Voice").map((c) => c.id),
          ...sv.dms.map((d) => d.channel.id),
        ],
        sid,
      );
      if (ids.length === 0) continue;
      const key = JSON.stringify([...ids].sort((a, b) => a - b));
      if (last.current.get(sid) === key) continue;
      last.current.set(sid, key);
      api.subscribeChannels(sid, ids).catch((e) => {
        last.current.delete(sid); // try again on the next change
        console.error("[subscribe] failed; channels may stop receiving messages:", e);
      });
    }
  }, [state.serverList, state.servers, tick]);
}
