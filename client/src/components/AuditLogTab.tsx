import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { AuditEvent } from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";

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

export default function AuditLogTab({ serverId }: Props) {
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
  if (error) return <div style={{ padding: 16, color: "#a00" }}>{error}</div>;
  if (events.length === 0) {
    return <div style={{ padding: 16, color: "var(--xp-text-muted, #666)" }}>No moderation actions recorded yet.</div>;
  }

  return (
    <div style={{ padding: 8 }}>
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
        <button onClick={loadOlder} style={{ marginTop: 8, font: "inherit", padding: "4px 12px" }}>
          Load older
        </button>
      )}
    </div>
  );
}
