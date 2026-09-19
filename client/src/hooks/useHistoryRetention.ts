import { useEffect } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

// ---------------------------------------------------------------------------
// Local retention sweep (sub-7a T4/T11, the half that was never wired).
//
// A channel with `retention_secs` set is a promise that messages older than that
// window are gone. The SERVER keeps that promise on its own copy — a background
// task purges expired rows — but for an E2EE channel the server's copy is
// ciphertext it cannot read, and the readable copy is the one on this device.
// So a retention window means nothing end to end unless this client sweeps its
// own store too. `history_purge_before` existed for exactly this and nothing
// called it.
//
// It has to be local rather than event-driven: the server's retention task
// broadcasts nothing (it is a silent background sweep), and a client that waited
// for a signal would wait forever. Everything needed is already here — the
// channel list carries `retention_secs`.
//
// Deliberately conservative in one direction only: the sweep can lag the
// server's (this device may be offline for a week), but it can never delete
// something the retention window still covers.
// ---------------------------------------------------------------------------

/** Re-sweep at most this often per channel, so a re-render or a channel-list
 *  refresh does not turn into a purge per keystroke. */
const SWEEP_INTERVAL_MS = 5 * 60 * 1000;

const lastSweep = new Map<string, number>();

export function useHistoryRetention(): void {
  const { state } = useApp();
  const activeServerId = state.activeServerId;
  const server = activeServerId ? state.servers[activeServerId] : undefined;
  const channels = server?.channels ?? [];

  useEffect(() => {
    if (!activeServerId) return;
    const nowMs = Date.now();
    const nowSecs = Math.floor(nowMs / 1000);

    for (const ch of channels) {
      const window = ch.retention_secs;
      if (!window || window <= 0) continue;

      const key = `${activeServerId}:${ch.id}`;
      const last = lastSweep.get(key) ?? 0;
      if (nowMs - last < SWEEP_INTERVAL_MS) continue;
      lastSweep.set(key, nowMs);

      // A window longer than the epoch would make the cutoff negative, and the
      // command takes an unsigned timestamp — the call would fail to
      // deserialize rather than do anything. Clamp instead: "before 0" purges
      // nothing, which is the right answer for a window that has not elapsed.
      const cutoff = Math.max(0, nowSecs - window);
      void api.historyPurgeBefore(ch.id, cutoff).catch((e) => {
        // A locked identity is the ordinary case here (the store cannot be
        // opened yet), not an error worth surfacing. The timestamp above is
        // recorded on the ATTEMPT, not on success, so a failure waits out the
        // full interval before trying again — deliberately, because the
        // alternative is retrying on every render while the identity stays
        // locked. Nothing was readable in the meantime either.
        console.warn("[history] retention sweep failed:", e);
      });
    }
  }, [activeServerId, channels]);
}
