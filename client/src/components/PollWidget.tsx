import { useEffect, useRef, useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { toast } from "../lib/toast";
import { publicKeyToString } from "../lib/types";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";

interface PollWidgetProps {
  serverId: string;
  pollId: number;
  /** Called when the poll can't be loaded (deleted/unknown) — parent falls back
   *  to rendering the message's plain-text content. */
  onUnavailable?: () => void;
  /** Freshness discipline for linked renders (PollUpdated broadcasts reach
   *  origin-channel subscribers only, so cards outside that channel go stale):
   *  - undefined (default): fetch only when absent from the slice;
   *  - "mount": always fetch once on mount (fresh counts + my_vote);
   *  - "interval": "mount" plus refetch after each own successful interaction
   *    ack plus a 20 s interval while mounted. */
  refetch?: "mount" | "interval";
}

// Module-level cache for own public key (same idiom as Message.tsx).
let cachedOwnPk: string | null = null;

/** "closes in 2h 10m"-style remaining-time formatter (unix-seconds delta). */
function formatRemaining(secs: number): string {
  if (secs <= 0) return "closing soon";
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return h > 0 ? `${d}d ${h}h` : `${d}d`;
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  if (m > 0) return `${m}m`;
  return "under a minute";
}

export default function PollWidget({ serverId, pollId, onUnavailable, refetch }: PollWidgetProps) {
  const { state, dispatch } = useApp();
  const server = state.servers[serverId] ?? null;
  const entry = server?.polls[pollId] ?? null;
  const poll = entry?.poll ?? null;
  const myVote = entry?.myVote ?? null;

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

  function fetchPoll() {
    api.getPoll(serverId, pollId)
      .then((s) => {
        dispatch({ type: "POLL_STATE", serverId, payload: { poll: s.poll, myVote: s.my_vote } });
      })
      .catch(() => { onUnavailable?.(); });
  }

  // State recovery: absent from the reducer → fetch once on mount.
  // refetch="mount"/"interval" always fetch on mount, even when the slice
  // already has the poll (refreshes counts + the per-viewer my_vote).
  useEffect(() => {
    if (fetchedRef.current) return;
    if (!refetch && entry) return;
    fetchedRef.current = true;
    fetchPoll();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entry, serverId, pollId]);

  // refetch="interval": 20 s poll for linked cards outside the origin channel
  // (they receive no PollUpdated events); cleared on unmount.
  useEffect(() => {
    if (refetch !== "interval") return;
    const t = window.setInterval(fetchPoll, 20_000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refetch, serverId, pollId]);

  // 30 s countdown tick while open + timed; re-derived from closes_at, no server ticks.
  useEffect(() => {
    if (!poll || poll.closed || poll.closes_at == null) return;
    setNowSecs(Math.floor(Date.now() / 1000));
    const t = window.setInterval(() => setNowSecs(Math.floor(Date.now() / 1000)), 30_000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [poll?.closed, poll?.closes_at]);

  // A fresh PollUpdated/getPoll snapshot supersedes any stale race error.
  useEffect(() => { setError(null); }, [poll]);

  if (!poll) return null; // still fetching (or unavailable — parent handles fallback)

  const total = poll.total_votes;
  const closed = poll.closed;

  // Winner highlight: closed-state argmax, ALL tied winners; none on zero votes.
  const winners = new Set<number>();
  if (closed && total > 0) {
    const max = Math.max(...poll.counts);
    poll.counts.forEach((c, i) => { if (c === max) winners.add(i); });
  }

  const creatorStr = publicKeyToString(poll.creator);
  const { bits } = ownPk && server
    ? getActorPermissions(server.members, server.roles, ownPk, server.ownerPublicKey)
    : { bits: 0n };
  const canClose =
    !closed && ownPk != null &&
    (creatorStr === ownPk || hasPermission(bits, PERMISSIONS.MANAGE_SERVER));

  async function handleOptionClick(index: number) {
    if (closed || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (myVote === index) {
        // Clicking my own voted option retracts the vote.
        await api.retractVote(serverId, pollId);
        dispatch({ type: "POLL_MY_VOTE", serverId, payload: { pollId, myVote: null } });
      } else {
        // First vote or re-vote onto a different option.
        await api.votePoll(serverId, pollId, index);
        dispatch({ type: "POLL_MY_VOTE", serverId, payload: { pollId, myVote: index } });
      }
      // Cross-channel linked cards get no PollUpdated broadcast — pull the
      // fresh counts right after our own ack instead of waiting for the tick.
      if (refetch === "interval") fetchPoll();
    } catch (e) {
      setError(String(e)); // e.g. lost race with close; next PollUpdated corrects
    } finally {
      setBusy(false);
    }
  }

  async function handleClose() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.closePoll(serverId, pollId); // PollUpdated broadcast flips the UI
      if (refetch === "interval") fetchPoll(); // no broadcast off-channel — refresh now
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  function copyWidgetLink() {
    if (!poll) return;
    const link = `farder://widget/poll/${poll.channel_id}/${poll.id}`;
    navigator.clipboard?.writeText(link).then(
      () => toast.success("Widget link copied"),
      () => {},
    );
  }

  const footerStatus = closed
    ? "final results"
    : poll.closes_at != null
      ? `closes in ${formatRemaining(poll.closes_at - nowSecs)}`
      : null;

  return (
    <div className="poll-widget">
      <div className="poll-question">{poll.question}</div>
      {poll.options.map((opt, i) => {
        const count = poll.counts[i] ?? 0;
        const pct = total > 0 ? Math.round((count / total) * 100) : 0;
        const classes = [
          "poll-option",
          myVote === i ? "poll-option--mine" : "",
          winners.has(i) ? "poll-option--winner" : "",
        ].filter(Boolean).join(" ");
        return (
          <button
            key={i}
            className={classes}
            disabled={closed || busy}
            onClick={() => handleOptionClick(i)}
          >
            <span className="poll-option-bar" style={{ width: `${pct}%` }} />
            <span className="poll-option-label">{opt}</span>
            <span className="poll-option-count">{count} · {pct}%</span>
          </button>
        );
      })}
      {error && <div className="error-text">{error}</div>}
      <div className="poll-footer">
        <span>
          {total} vote{total === 1 ? "" : "s"}
          {footerStatus ? ` · ${footerStatus}` : ""}
        </span>
        <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
          {canClose && (
            <button className="xp-button" onClick={handleClose} disabled={busy}>
              Close poll
            </button>
          )}
          <button className="widget-copy-link" title="Copy widget link" onClick={copyWidgetLink}>
            🔗
          </button>
        </span>
      </div>
    </div>
  );
}
