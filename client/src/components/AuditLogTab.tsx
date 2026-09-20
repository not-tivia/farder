import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { AuditEvent } from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import ActivityLogView from "./ActivityLogView";

interface Props {
  serverId: string;
}

const ACTION_VERBS: Record<string, (target: string | null, meta: Record<string, unknown>) => string> = {
  kick: (t) => `kicked ${t ?? ""}`,
  ban: (t) => `banned ${t ?? ""}`,
  unban: (t) => `unbanned ${t ?? ""}`,
  timeout: (t, m) => {
    const u = m["until_ms"] as number | undefined;
    return `timed out ${t ?? ""}${u ? ` until ${new Date(u).toLocaleString()}` : ""}`;
  },
  untimeout: (t) => `removed timeout from ${t ?? ""}`,
  role_assigned: (t, m) => `assigned role "${m["role_name"] ?? "?"}" to ${t ?? ""}`,
  role_removed: (t, m) => `removed role "${m["role_name"] ?? "?"}" from ${t ?? ""}`,
  channel_created: (_t, m) => `created channel #${m["channel_name"] ?? "?"}`,
  channel_deleted: (_t, m) => `deleted channel #${m["channel_name"] ?? "?"}`,
  channel_renamed: (_t, m) => `renamed channel #${m["old_name"] ?? "?"} → #${m["new_name"] ?? "?"}`,
  role_created: (_t, m) => `created role "${m["role_name"] ?? "?"}"`,
  role_deleted: (_t, m) => `deleted role "${m["role_name"] ?? "?"}"`,
  role_perms_changed: (_t, m) => `changed permissions for role id ${m["role_id"] ?? "?"}`,
  channel_overrides_changed: (_t, m) => `changed channel overrides on channel ${m["channel_id"] ?? "?"} for role ${m["role_id"] ?? "?"}`,
  member_joined: (_t, m) => {
    const via = m["via"];
    if (via === "first_member") return "claimed the server as its first member";
    if (via === "setup_token") return "joined with the setup token";
    const code = m["invite_code"];
    return code ? `joined on invite ${code}` : "joined the server";
  },
  activity_logging_changed: (_t, m) =>
    m["enabled"]
      ? `turned join/leave recording ON (kept ${m["retention_days"] ?? "?"} days)`
      : "turned join/leave recording OFF",
};

function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return `${Math.floor(diff / 86_400_000)}d ago`;
}

const row: CSSProperties = {
  padding: "8px 4px",
  borderBottom: "1px solid var(--xp-border, #ddd)",
  fontSize: 11,
  cursor: "pointer",
};

const detail: CSSProperties = {
  background: "var(--xp-panel-bg, #fafafa)",
  padding: 8,
  marginTop: 4,
  fontSize: 10,
  fontFamily: "monospace",
  whiteSpace: "pre-wrap",
};

/**
 * The audit log, in two halves.
 *
 * They are separate lists on the server and separate views here for one
 * reason: volume. A moderation row is a deliberate act by someone with power
 * and there are a handful a week. An activity row is written by ordinary use
 * and an active server produces hundreds a day. Merged, the joins bury the bans
 * inside an hour — which is the same as not having a moderation log.
 *
 * Moderation is the default view because it is the one an admin opens under
 * pressure.
 */
export default function AuditLogTab({ serverId }: Props) {
  const [view, setView] = useState<"moderation" | "activity">("moderation");

  return (
    <div style={{ padding: 8 }}>
      <div style={{ display: "flex", gap: 6, marginBottom: 10 }}>
        <button
          className="xp-button"
          disabled={view === "moderation"}
          onClick={() => setView("moderation")}
        >
          Moderation
        </button>
        <button
          className="xp-button"
          disabled={view === "activity"}
          onClick={() => setView("activity")}
        >
          Joins &amp; voice
        </button>
      </div>
      {view === "moderation"
        ? <ModerationLogView serverId={serverId} />
        : <ActivityLogView serverId={serverId} />}
    </div>
  );
}

function ModerationLogView({ serverId }: Props) {
  const activeServer = useActiveServer();
  const [events, setEvents] = useState<AuditEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(true);
  const [expandedId, setExpandedId] = useState<number | null>(null);
  const seenIds = useRef<Set<number>>(new Set());

  const memberByPk = useMemo(() => {
    const map = new Map<string, string>();
    activeServer?.members.forEach((m) => map.set(publicKeyToString(m.public_key), m.display_name));
    return map;
  }, [activeServer?.members]);

  function nameFor(pk: string | null): string | null {
    if (!pk) return null;
    return memberByPk.get(pk) ?? pk.slice(0, 8) + "…";
  }

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api.listAuditEvents(serverId, null, 50)
      .then((evts) => {
        if (cancelled) return;
        evts.forEach((e) => seenIds.current.add(e.id));
        setEvents(evts);
        setHasMore(evts.length === 50);
      })
      .catch((e) => { if (!cancelled) setError(String(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [serverId]);

  // Live updates
  useEffect(() => {
    function onAudit(e: Event) {
      const detail = (e as CustomEvent).detail as { server_id: string; event: AuditEvent };
      if (detail.server_id !== serverId) return;
      if (seenIds.current.has(detail.event.id)) return;
      seenIds.current.add(detail.event.id);
      setEvents((prev) => [detail.event, ...prev]);
    }
    window.addEventListener("farder:audit-event-created", onAudit);
    return () => window.removeEventListener("farder:audit-event-created", onAudit);
  }, [serverId]);

  async function loadOlder() {
    if (!hasMore || events.length === 0) return;
    const oldest = events[events.length - 1].id;
    try {
      const more = await api.listAuditEvents(serverId, oldest, 50);
      more.forEach((e) => seenIds.current.add(e.id));
      setEvents((prev) => [...prev, ...more]);
      setHasMore(more.length === 50);
    } catch (e) {
      setError(String(e));
    }
  }

  if (loading) return <div style={{ padding: 16 }}>Loading audit log…</div>;
  if (error) return <div className="error-text" style={{ padding: 16 }}>{error}</div>;
  if (events.length === 0) {
    return <div style={{ padding: 16, color: "var(--xp-text-muted, #666)" }}>No moderation actions recorded yet.</div>;
  }

  return (
    <div>
      {events.map((evt) => {
        const actorName = nameFor(publicKeyToString(evt.actor)) ?? "?";
        const targetName = evt.target ? nameFor(publicKeyToString(evt.target)) : null;
        const verb = ACTION_VERBS[evt.action]
          ? ACTION_VERBS[evt.action](targetName, evt.metadata)
          : `did "${evt.action}"`;
        const expanded = expandedId === evt.id;
        return (
          <div
            key={evt.id}
            style={row}
            onClick={() => setExpandedId(expanded ? null : evt.id)}
          >
            <div>
              <strong>{actorName}</strong> {verb}{" "}
              <span style={{ color: "var(--xp-text-muted, #666)" }}>· {relativeTime(evt.timestamp_ms)}</span>
            </div>
            {expanded && <pre style={detail}>{JSON.stringify(evt.metadata, null, 2)}</pre>}
          </div>
        );
      })}
      {hasMore && (
        <button className="xp-button" onClick={loadOlder} style={{ marginTop: 8 }}>
          Load older
        </button>
      )}
    </div>
  );
}
