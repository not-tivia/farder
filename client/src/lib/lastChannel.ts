// Remembers the last text channel viewed per server (client-side) so reconnecting
// to a server reopens the channel you were in. Fails safe to null.

const PREFIX = "farder.lastChannel.";

export function getLastChannel(serverId: string): number | null {
  try {
    const v = localStorage.getItem(PREFIX + serverId);
    const n = v ? parseInt(v, 10) : NaN;
    return Number.isFinite(n) ? n : null;
  } catch { return null; }
}

export function setLastChannel(serverId: string, channelId: number): void {
  try { localStorage.setItem(PREFIX + serverId, String(channelId)); } catch { /* ignore */ }
}
