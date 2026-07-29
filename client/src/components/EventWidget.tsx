import { useEffect, useRef, useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { toast } from "../lib/toast";
import { publicKeyToString } from "../lib/types";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";
import EventBuilderModal from "./EventBuilderModal";

interface EventWidgetProps {
  serverId: string;
  eventId: number;
  /** Called when the event can't be loaded (deleted/unknown) — parent falls
   *  back to rendering the message's plain-text content. */
  onUnavailable?: () => void;
  /** Freshness discipline for linked renders (EventUpdated broadcasts reach
   *  origin-channel subscribers only, so cards outside that channel go stale):
   *  - undefined (default): fetch only when absent from the slice;
   *  - "mount": always fetch once on mount (fresh roster + my_rsvp);
   *  - "interval": "mount" plus refetch after each own successful interaction
   *    ack plus a 20 s interval while mounted. */
  refetch?: "mount" | "interval";
}

// Module-level cache for own public key (same idiom as PollWidget/GiveawayWidget).
let cachedOwnPk: string | null = null;

/** How long after the start a 'started' event still reads as "Happening now"
 *  before it settles into a past-tense "Started". (The local time itself is
 *  never repeated here — `.event-when` below always renders it, exactly once.) */
const HAPPENING_WINDOW_SECS = 3 * 3600;

/** "in 3 days"-style lead-time hint (unix-seconds delta until the start). */
function formatRelative(secs: number): string {
  if (secs <= 0) return "starting now";
  const d = Math.floor(secs / 86400);
  if (d >= 1) return `in ${d} day${d === 1 ? "" : "s"}`;
  const h = Math.floor(secs / 3600);
  if (h >= 1) return `in ${h} hour${h === 1 ? "" : "s"}`;
  const m = Math.floor(secs / 60);
  if (m >= 1) return `in ${m} minute${m === 1 ? "" : "s"}`;
  return "in under a minute";
}

const RSVP_OPTIONS: { value: string; label: string }[] = [
  { value: "going", label: "Going" },
  { value: "maybe", label: "Maybe" },
  { value: "no", label: "Can't make it" },
];

export default function EventWidget({ serverId, eventId, onUnavailable, refetch }: EventWidgetProps) {
  const { state, dispatch } = useApp();
  const server = state.servers[serverId] ?? null;
  const entry = server?.events[eventId] ?? null;
  const event = entry?.event ?? null;
  const myRsvp = entry?.myRsvp ?? null;

  const [ownPk, setOwnPk] = useState(cachedOwnPk);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editOpen, setEditOpen] = useState(false);
  const [nowSecs, setNowSecs] = useState(() => Math.floor(Date.now() / 1000));
  const fetchedRef = useRef(false);

  useEffect(() => {
    if (!cachedOwnPk) {
      api.getPublicKey().then((pk) => { cachedOwnPk = pk; setOwnPk(pk); });
    }
  }, []);

  function fetchEvent() {
    api.getEvent(serverId, eventId)
      .then((s) => {
        dispatch({ type: "EVENT_STATE", serverId, payload: { event: s.event, myRsvp: s.my_rsvp } });
      })
      .catch(() => { onUnavailable?.(); });
  }

  // State recovery: absent from the reducer → fetch once on mount.
  // refetch="mount"/"interval" always fetch on mount, even when the slice
  // already has the event (refreshes the roster + the per-viewer my_rsvp).
  useEffect(() => {
    if (fetchedRef.current) return;
    if (!refetch && entry) return;
    fetchedRef.current = true;
    fetchEvent();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entry, serverId, eventId]);

  // refetch="interval": 20 s poll for linked cards outside the origin channel
  // (they receive no EventUpdated events); cleared on unmount.
  useEffect(() => {
    if (refetch !== "interval") return;
    const t = window.setInterval(fetchEvent, 20_000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refetch, serverId, eventId]);

  // 30 s tick for the relative "in 3 days" hint (the PollWidget footer idiom);
  // re-derived from starts_at, no server ticks. Cancelled cards need no tick.
  useEffect(() => {
    if (!event || event.status === "cancelled") return;
    setNowSecs(Math.floor(Date.now() / 1000));
    const t = window.setInterval(() => setNowSecs(Math.floor(Date.now() / 1000)), 30_000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [event?.status]);

  // A fresh EventUpdated/getEvent snapshot supersedes any stale race error.
  useEffect(() => { setError(null); }, [event]);

  if (!event) return null; // still fetching (or unavailable — parent handles fallback)

  const cancelled = event.status === "cancelled";
  const started = event.status === "started";
  // The server refuses an RSVP once the start passed, even before the sweeper
  // flips the status — mirror that cutoff so the buttons don't lie.
  const rsvpOpen = !cancelled && !started && nowSecs < event.starts_at;

  const creatorStr = publicKeyToString(event.creator);
  const { bits } = ownPk && server
    ? getActorPermissions(server.members, server.roles, ownPk, server.ownerPublicKey)
    : { bits: 0n };
  // Server re-checks regardless — this only decides whether to render them.
  const canManage =
    !cancelled && !started && ownPk != null &&
    (creatorStr === ownPk || hasPermission(bits, PERMISSIONS.MANAGE_SERVER));

  async function handleRsvp(response: string) {
    if (!rsvpOpen || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (myRsvp === response) {
        // Pressing my own answer withdraws it (the poll-retract idiom).
        await api.clearRsvp(serverId, eventId);
        dispatch({ type: "EVENT_MY_RSVP", serverId, payload: { eventId, myRsvp: null } });
      } else {
        await api.rsvpEvent(serverId, eventId, response);
        dispatch({ type: "EVENT_MY_RSVP", serverId, payload: { eventId, myRsvp: response } });
      }
      // Cross-channel linked cards get no EventUpdated broadcast — pull the
      // fresh roster right after our own ack instead of waiting for the tick.
      if (refetch === "interval") fetchEvent();
    } catch (e) {
      setError(String(e)); // e.g. lost race with the start; next EventUpdated corrects
    } finally {
      setBusy(false);
    }
  }

  async function handleCancel() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.cancelEvent(serverId, eventId); // EventUpdated broadcast flips the UI
      if (refetch === "interval") fetchEvent(); // no broadcast off-channel — refresh now
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  function copyWidgetLink() {
    if (!event) return;
    const link = `farder://widget/event/${event.channel_id}/${event.id}`;
    navigator.clipboard?.writeText(link).then(
      () => toast.success("Widget link copied"),
      () => {},
    );
  }

  const whenLocal = new Date(event.starts_at * 1000).toLocaleString();
  const counts: Record<string, number> = {
    going: event.going_count,
    maybe: event.maybe_count,
    no: event.no_count,
  };
  const names: Record<string, string[]> = {
    going: event.going_names,
    maybe: event.maybe_names,
    no: event.no_names,
  };

  return (
    <div className="event-widget">
      <div
        className="event-title"
        style={cancelled ? { textDecoration: "line-through" } : undefined}
      >
        📅 {event.title}
      </div>

      {cancelled && <div className="event-cancelled">Event cancelled</div>}

      {started && (
        <div className="event-happening">
          {nowSecs - event.starts_at < HAPPENING_WINDOW_SECS ? "Happening now" : "Started"}
        </div>
      )}

      <div className="event-when">
        {whenLocal}
        {!cancelled && !started && (
          <span className="event-when-rel">{formatRelative(event.starts_at - nowSecs)}</span>
        )}
      </div>

      {event.location && <div className="event-location">📍 {event.location}</div>}
      {event.description && <div className="event-description">{event.description}</div>}

      <div className="event-rsvp-row">
        {RSVP_OPTIONS.map((o) => (
          <button
            key={o.value}
            className={`event-rsvp-btn${myRsvp === o.value ? " event-rsvp-btn--mine" : ""}`}
            disabled={!rsvpOpen || busy}
            onClick={() => handleRsvp(o.value)}
          >
            {o.label}
          </button>
        ))}
      </div>

      <div className="event-attendees">
        {RSVP_OPTIONS.map((o) => {
          const count = counts[o.value] ?? 0;
          const shown = names[o.value] ?? [];
          const more = count - shown.length;
          return (
            <div key={o.value} className="event-attendee-group">
              <strong>{o.label} · {count}</strong>
              {shown.map((n, i) => (
                <div key={i} className="event-attendee-name">{n}</div>
              ))}
              {more > 0 && <div className="event-more">and {more} more</div>}
            </div>
          );
        })}
      </div>

      {error && <div className="error-text">{error}</div>}

      {/* Footer mirroring PollWidget/GiveawayWidget: 🔗 stays reachable in every
          state so a started/cancelled event is still linkable. */}
      <div className="poll-footer">
        <span>{event.remind_lead != null ? "reminder set" : ""}</span>
        <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
          {canManage && (
            <>
              <button className="xp-button" onClick={() => setEditOpen(true)} disabled={busy}>
                Edit
              </button>
              <button className="xp-button" onClick={handleCancel} disabled={busy}>
                Cancel
              </button>
            </>
          )}
          <button className="widget-copy-link" title="Copy widget link" onClick={copyWidgetLink}>
            🔗
          </button>
        </span>
      </div>

      {editOpen && (
        <EventBuilderModal
          serverId={serverId}
          channelId={event.channel_id}
          editing={event}
          onClose={() => setEditOpen(false)}
          onCreated={() => { setEditOpen(false); if (refetch === "interval") fetchEvent(); }}
        />
      )}
    </div>
  );
}
