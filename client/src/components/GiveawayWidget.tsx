import { useEffect, useRef, useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { toast } from "../lib/toast";
import { publicKeyToString } from "../lib/types";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";

interface GiveawayWidgetProps {
  serverId: string;
  giveawayId: number;
  /** Called when the giveaway can't be loaded (deleted/unknown) — parent falls
   *  back to rendering the message's plain-text content. */
  onUnavailable?: () => void;
  /** Freshness discipline for linked renders (GiveawayUpdated broadcasts reach
   *  origin-channel subscribers only, so cards outside that channel go stale):
   *  - undefined (default): fetch only when absent from the slice;
   *  - "mount": always fetch once on mount (fresh count + my_entered);
   *  - "interval": "mount" plus refetch after each own successful interaction
   *    ack plus a 20 s interval while mounted. */
  refetch?: "mount" | "interval";
}

// Module-level cache for own public key (same idiom as Message.tsx/PollWidget.tsx).
let cachedOwnPk: string | null = null;

/** "2h 10m"-style remaining-time formatter (unix-seconds delta; 1 s tick, so
 *  the final minute counts down in seconds). */
function formatRemaining(secs: number): string {
  if (secs <= 0) return "ending soon";
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (d > 0) return h > 0 ? `${d}d ${h}h` : `${d}d`;
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  if (m > 0) return `${m}m ${String(s).padStart(2, "0")}s`;
  return `${s}s`;
}

/** Short display form of a "vk_<hex>" key string (matches the server's
 *  announcement fallback: "vk_" + 8 hex chars). */
function shortKey(pk: string): string {
  return pk.slice(0, 11);
}

export default function GiveawayWidget({ serverId, giveawayId, onUnavailable, refetch }: GiveawayWidgetProps) {
  const { state, dispatch } = useApp();
  const server = state.servers[serverId] ?? null;
  const entry = server?.giveaways[giveawayId] ?? null;
  const giveaway = entry?.giveaway ?? null;
  const myEntered = entry?.myEntered ?? false;

  const [ownPk, setOwnPk] = useState(cachedOwnPk);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [nowSecs, setNowSecs] = useState(() => Math.floor(Date.now() / 1000));
  const fetchedRef = useRef(false);

  useEffect(() => {
    if (!cachedOwnPk) {
      api.getPublicKey().then((pk) => { cachedOwnPk = pk; setOwnPk(pk); });
    }
  }, []);

  function fetchGiveaway() {
    api.getGiveaway(serverId, giveawayId)
      .then((s) => {
        dispatch({
          type: "GIVEAWAY_STATE",
          serverId,
          payload: { giveaway: s.giveaway, myEntered: s.my_entered },
        });
      })
      .catch(() => { onUnavailable?.(); });
  }

  // State recovery: absent from the reducer → fetch once on mount.
  // refetch="mount"/"interval" always fetch on mount, even when the slice
  // already has the giveaway (refreshes the count + the per-viewer my_entered).
  useEffect(() => {
    if (fetchedRef.current) return;
    if (!refetch && entry) return;
    fetchedRef.current = true;
    fetchGiveaway();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entry, serverId, giveawayId]);

  // refetch="interval": 20 s poll for linked cards outside the origin channel
  // (they receive no GiveawayUpdated events); cleared on unmount.
  useEffect(() => {
    if (refetch !== "interval") return;
    const t = window.setInterval(fetchGiveaway, 20_000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refetch, serverId, giveawayId]);

  // Live 1 s countdown while open; re-derived from ends_at, no server ticks.
  useEffect(() => {
    if (!giveaway || giveaway.status !== "open") return;
    setNowSecs(Math.floor(Date.now() / 1000));
    const t = window.setInterval(() => setNowSecs(Math.floor(Date.now() / 1000)), 1_000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [giveaway?.status]);

  // A fresh GiveawayUpdated/getGiveaway snapshot supersedes any stale race error.
  useEffect(() => { setError(null); }, [giveaway]);

  if (!giveaway) return null; // still fetching (or unavailable — parent handles fallback)

  const creatorStr = publicKeyToString(giveaway.creator);
  const { bits } = ownPk && server
    ? getActorPermissions(server.members, server.roles, ownPk, server.ownerPublicKey)
    : { bits: 0n };
  const canModerate =
    ownPk != null &&
    (creatorStr === ownPk || hasPermission(bits, PERMISSIONS.MANAGE_SERVER));

  async function handleToggleEntry() {
    if (busy || !giveaway || giveaway.status !== "open") return;
    setBusy(true);
    setError(null);
    try {
      if (myEntered) {
        await api.leaveGiveaway(serverId, giveawayId);
        dispatch({ type: "GIVEAWAY_MY_ENTERED", serverId, payload: { giveawayId, myEntered: false } });
      } else {
        await api.enterGiveaway(serverId, giveawayId);
        dispatch({ type: "GIVEAWAY_MY_ENTERED", serverId, payload: { giveawayId, myEntered: true } });
      }
      // Cross-channel linked cards get no GiveawayUpdated broadcast — pull the
      // fresh count right after our own ack instead of waiting for the tick.
      if (refetch === "interval") fetchGiveaway();
    } catch (e) {
      setError(String(e)); // e.g. lost race with the draw; next GiveawayUpdated corrects
    } finally {
      setBusy(false);
    }
  }

  async function handleCancel() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.cancelGiveaway(serverId, giveawayId); // GiveawayUpdated broadcast flips the UI
      if (refetch === "interval") fetchGiveaway(); // no broadcast off-channel — refresh now
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleReroll() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.rerollGiveaway(serverId, giveawayId); // GiveawayUpdated + announcement follow
      if (refetch === "interval") fetchGiveaway(); // no broadcast off-channel — refresh now
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  function copyWidgetLink() {
    if (!giveaway) return;
    const link = `farder://widget/giveaway/${giveaway.channel_id}/${giveaway.id}`;
    navigator.clipboard?.writeText(link).then(
      () => toast.success("Widget link copied"),
      () => {},
    );
  }

  const entries = giveaway.entry_count;

  return (
    <div className="giveaway-widget">
      {/* The copy-link control lives on the always-rendered prize row so ended/
          cancelled giveaways stay linkable (mirrors PollWidget's footer 🔗). */}
      <div className="giveaway-prize" style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span>🎉 {giveaway.prize}</span>
        <button className="widget-copy-link" title="Copy widget link" onClick={copyWidgetLink}>
          🔗
        </button>
      </div>

      {giveaway.status === "open" && (
        <>
          <div className="giveaway-meta">
            ends in {formatRemaining(giveaway.ends_at - nowSecs)} · {entries} entr{entries === 1 ? "y" : "ies"}
          </div>
          <div className="giveaway-actions">
            <button
              className="giveaway-enter-btn"
              onClick={handleToggleEntry}
              disabled={busy}
            >
              {myEntered ? "Leave" : "Enter"}
            </button>
            {canModerate && (
              <button className="giveaway-action-link" onClick={handleCancel} disabled={busy}>
                Cancel
              </button>
            )}
          </div>
        </>
      )}

      {giveaway.status === "ended" && (
        <>
          <div className="giveaway-winner">
            {giveaway.winner
              ? `🎉 Winner: ${giveaway.winner_name ?? shortKey(giveaway.winner)}`
              : "No entries."}
          </div>
          {canModerate && giveaway.winner && (
            <div className="giveaway-actions">
              <button className="giveaway-action-link" onClick={handleReroll} disabled={busy}>
                Reroll
              </button>
            </div>
          )}
        </>
      )}

      {giveaway.status === "cancelled" && (
        <div className="giveaway-cancelled">Giveaway cancelled.</div>
      )}

      {error && <div className="error-text">{error}</div>}
    </div>
  );
}
