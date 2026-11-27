// Channels that a small chat window is showing, per server.
//
// The server only sends live messages for channels the app has SUBSCRIBED to, and
// a subscribe call REPLACES the earlier list (see crates/farder-server/src/
// subscriptions.rs). The main window subscribes to the channel it is viewing, so a
// small chat window for any other channel never heard about new messages. Whoever
// subscribes must therefore include these too.

/** Fired (on `window`) when the set changes, so the main window can subscribe again. */
export const WATCHED_CHANGED_EVENT = "farder:watched-channels-changed";

const watched = new Map<string, Set<number>>();

export function setWatched(serverId: string, channelIds: number[]): void {
  const before = JSON.stringify(watchedFor(serverId).sort());
  if (channelIds.length === 0) watched.delete(serverId);
  else watched.set(serverId, new Set(channelIds));
  if (JSON.stringify(watchedFor(serverId).sort()) !== before) window.dispatchEvent(new Event(WATCHED_CHANGED_EVENT));
}

export function watchedFor(serverId: string): number[] {
  return [...(watched.get(serverId) ?? [])];
}

/** The ids to subscribe to: what the main window shows plus the watched ones, no repeats, no nulls. */
export function subscriptionIds(shown: (number | null | undefined)[], serverId: string): number[] {
  const out: number[] = [];
  for (const id of [...shown, ...watchedFor(serverId)]) {
    if (typeof id === "number" && !out.includes(id)) out.push(id);
  }
  return out;
}
