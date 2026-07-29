import { useEffect, useRef, useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { useClickAnchoredPosition } from "../lib/useClickAnchoredPosition";
import PollWidget from "./PollWidget";
import GiveawayWidget from "./GiveawayWidget";
import EventWidget from "./EventWidget";

interface ActiveWidgetsBarProps {
  serverId: string;
  /** The channel currently shown in ChatPanel (server channel or DM). */
  channelId: number;
}

interface Chip {
  kind: "poll" | "giveaway" | "event";
  id: number;
  label: string;
  /** Unix-secs end time; null = untimed poll. */
  endsAt: number | null;
  /** Creation-order sort key (see the merge note in the component). */
  order: number;
}

/** Chip time-left: "2h 10m", "no end" for untimed polls, "ending…" when past
 *  due but not yet swept (the sweeper's broadcast drops the chip within ~15 s). */
function formatChipTime(endsAt: number | null, nowSecs: number): string {
  if (endsAt == null) return "no end";
  const secs = endsAt - nowSecs;
  if (secs <= 0) return "ending…";
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return h > 0 ? `${d}d ${h}h` : `${d}d`;
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  if (m > 0) return `${m}m`;
  return "under 1m";
}

/**
 * Chip strip under the channel header listing the channel's OPEN widgets.
 * Fetches via `listActiveWidgets` on channel switch/reconnect; chips
 * appear/disappear live off the `POLL_UPDATED`/`GIVEAWAY_UPDATED` reducer
 * maintenance (the viewed channel is subscribed by definition). Clicking a
 * chip opens one anchored dropdown hosting the full interactive widget with
 * `refetch="mount"` (live events keep it fresh; the mount fetch pulls the
 * per-viewer my_vote/my_entered the bar's upserts don't carry).
 */
export default function ActiveWidgetsBar({ serverId, channelId }: ActiveWidgetsBarProps) {
  const { state, dispatch } = useApp();
  const server = state.servers[serverId] ?? null;
  const connected = server?.connected ?? false;
  const activeWidgets = server?.activeWidgets ?? null;

  const [open, setOpen] = useState<{ kind: "poll" | "giveaway" | "event"; id: number; click: { x: number; y: number } } | null>(null);
  const [nowSecs, setNowSecs] = useState(() => Math.floor(Date.now() / 1000));
  const barRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Fetch on channel switch + reconnect (the subscribe-effect idiom). Errors
  // dispatch empty lists: the bar simply hides — opaque "channel not found"
  // is never surfaced, consistent with not seeing the channel's widgets.
  useEffect(() => {
    if (!connected) return;
    let cancelled = false;
    api.listActiveWidgets(serverId, channelId)
      .then((res) => {
        if (cancelled) return;
        dispatch({ type: "ACTIVE_WIDGETS", serverId, payload: { channelId, polls: res.polls, giveaways: res.giveaways, events: res.events } });
      })
      .catch(() => {
        if (cancelled) return;
        dispatch({ type: "ACTIVE_WIDGETS", serverId, payload: { channelId, polls: [], giveaways: [], events: [] } });
      });
    return () => { cancelled = true; };
  }, [serverId, connected, channelId, dispatch]);

  // A channel/server switch invalidates the anchored dropdown.
  useEffect(() => { setOpen(null); }, [serverId, channelId]);

  // Outside-click / Esc closes the dropdown. Chip clicks are excluded — the
  // chip's own toggle handles them (otherwise mousedown-close + click-reopen
  // would make clicking the open chip a no-op).
  useEffect(() => {
    if (!open) return;
    function handleMouse(e: MouseEvent) {
      const t = e.target as Node;
      if (dropdownRef.current?.contains(t)) return;
      if (barRef.current?.contains(t)) return;
      setOpen(null);
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(null);
    }
    document.addEventListener("mousedown", handleMouse);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleMouse);
      document.removeEventListener("keydown", handleKey);
    };
  }, [open]);

  const dropdownStyle = useClickAnchoredPosition(dropdownRef, open?.click ?? { x: 0, y: 0 }, {
    elementW: 340,
    elementH: 160,
    capHeight: true,
  });

  // Build chips from the id lists, looking each info up in the shared
  // polls/giveaways/events slices (one source of truth with the widgets).
  // Interleave in creation order by message_id: the three lists arrive id ASC
  // and the server merges by created_at, but GiveawayInfo/EventInfo carry no
  // created_at — message ids are one monotonically increasing sequence, so
  // message_id ascending IS creation order across all three kinds.
  const chips: Chip[] = [];
  if (activeWidgets && activeWidgets.channelId === channelId && server) {
    for (const id of activeWidgets.polls) {
      const p = server.polls[id]?.poll;
      if (p) chips.push({ kind: "poll", id, label: p.question, endsAt: p.closes_at, order: p.message_id });
    }
    for (const id of activeWidgets.giveaways) {
      const g = server.giveaways[id]?.giveaway;
      if (g) chips.push({ kind: "giveaway", id, label: g.prize, endsAt: g.ends_at, order: g.message_id });
    }
    for (const id of activeWidgets.events) {
      const ev = server.events[id]?.event;
      if (ev) chips.push({ kind: "event", id, label: ev.title, endsAt: ev.starts_at, order: ev.message_id });
    }
    chips.sort((a, b) => a.order - b.order);
  }
  const hasChips = chips.length > 0;

  // 30 s time-left tick while any chip shows (the PollWidget footer idiom).
  useEffect(() => {
    if (!hasChips) return;
    setNowSecs(Math.floor(Date.now() / 1000));
    const t = window.setInterval(() => setNowSecs(Math.floor(Date.now() / 1000)), 30_000);
    return () => window.clearInterval(t);
  }, [hasChips]);

  // Zero layout cost for widget-free channels — but keep an open dropdown
  // alive even after its chip dropped (the widget shows its final state; no
  // rug-pull mid-read).
  if (!hasChips && !open) return null;

  function toggleChip(kind: "poll" | "giveaway" | "event", id: number, e: React.MouseEvent) {
    setOpen(open && open.kind === kind && open.id === id
      ? null
      : { kind, id, click: { x: e.clientX, y: e.clientY } });
  }

  return (
    <>
      {hasChips && (
        <div className="active-widgets-bar" ref={barRef}>
          {chips.map((c) => (
            <button
              key={`${c.kind}-${c.id}`}
              className="widget-chip"
              onClick={(e) => toggleChip(c.kind, c.id, e)}
            >
              <span>{c.kind === "poll" ? "📊" : c.kind === "giveaway" ? "🎉" : "📅"}</span>
              <span style={{ maxWidth: "24ch", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {c.label}
              </span>
              <span className="widget-chip-time">{formatChipTime(c.endsAt, nowSecs)}</span>
            </button>
          ))}
        </div>
      )}
      {open && (
        <div className="widget-chip-dropdown" ref={dropdownRef} style={dropdownStyle}>
          {/* Keyed per widget: a chip-to-chip switch of the same kind must
              REMOUNT (not reconcile) so the widget's one-shot fetchedRef guard
              re-runs the refetch="mount" fetch for the new id — otherwise the
              new widget renders without its per-viewer my_vote/my_entered. */}
          {open.kind === "poll"
            ? <PollWidget key={`poll-${open.id}`} serverId={serverId} pollId={open.id} refetch="mount" />
            : open.kind === "giveaway"
              ? <GiveawayWidget key={`giveaway-${open.id}`} serverId={serverId} giveawayId={open.id} refetch="mount" />
              : <EventWidget key={`event-${open.id}`} serverId={serverId} eventId={open.id} refetch="mount" />}
        </div>
      )}
    </>
  );
}
