import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { AuditEvent } from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";

/**
 * The activity half of the audit log: who joined, who left, who was in which
 * voice channel.
 *
 * Kept apart from the moderation log on the server, and shown apart here, for
 * one reason: **volume**. A moderation row is a deliberate act by someone with
 * power and there are a handful a week; an activity row is written by ordinary
 * use and an active server produces hundreds a day. Merged into one list at
 * fifty rows a page, the joins would push every ban off the first page within
 * the hour, which is the same thing as not having a moderation log at all.
 *
 * Two things this view must keep saying out loud, because they are the reasons
 * the feature is defensible in a privacy product:
 *
 *  - **DM calls are never here.** A server voice channel is a public place
 *    whose name the server already knows. A record of who called whom, and for
 *    how long, is a call-detail record, and the server refuses to write one
 *    (`voice_activity_is_loggable` in the server's handlers).
 *  - **It expires.** Unlike the moderation log, these rows are swept on a
 *    window. The window is shown next to the switch rather than buried, because
 *    "we keep this for 30 days" is a promise a reader should be able to check.
 */

const ACTIVITY_VERBS: Record<string, (meta: Record<string, unknown>) => string> = {
  member_joined: (m) => {
    const via = m["via"];
    if (via === "first_member") return "claimed the server as its first member";
    if (via === "setup_token") return "joined with the setup token";
    return "joined the server";
  },
  session_started: () => "connected",
  session_ended: () => "disconnected",
  voice_joined: (m) => `joined voice in ${m["channel_name"] ?? "a channel"}`,
  voice_left: (m) => `left voice in ${m["channel_name"] ?? "a channel"}`,
};

function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return `${Math.floor(diff / 86_400_000)}d ago`;
}

const rowStyle: CSSProperties = {
  padding: "8px 4px",
  borderBottom: "1px solid var(--xp-border)",
  fontSize: 11,
};

export default function ActivityLogView({ serverId }: { serverId: string }) {
  const activeServer = useActiveServer();
  const [events, setEvents] = useState<AuditEvent[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [settings, setSettings] = useState<api.ActivityLogSettings | null>(null);
  const [savingSettings, setSavingSettings] = useState(false);
  const [draftDays, setDraftDays] = useState("30");
  const refreshTimer = useRef<number | null>(null);

  const memberByPk = useMemo(() => {
    const map = new Map<string, string>();
    activeServer?.members.forEach((m) => map.set(publicKeyToString(m.public_key), m.display_name));
    return map;
  }, [activeServer?.members]);

  const refresh = useCallback(async () => {
    try {
      const evts = await api.listActivityEvents(serverId, null, 50);
      setEvents(evts);
      setHasMore(evts.length === 50);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, [serverId]);

  useEffect(() => {
    void refresh();
    api.getActivityLogging(serverId)
      .then((s) => { setSettings(s); setDraftDays(String(s.retention_days)); })
      .catch((e) => setError(String(e)));
  }, [serverId, refresh]);

  // Live, without putting a single extra byte on the wire: the client already
  // receives `MediaJoined`/`MediaLeft` for the roster, so a join it has just
  // rendered in the sidebar is a join the log has just recorded. Debounced
  // because a call emptying out fires one per person.
  useEffect(() => {
    function onActivity() {
      if (refreshTimer.current !== null) window.clearTimeout(refreshTimer.current);
      refreshTimer.current = window.setTimeout(() => { void refresh(); }, 1200);
    }
    window.addEventListener("farder:voice-activity", onActivity);
    return () => {
      window.removeEventListener("farder:voice-activity", onActivity);
      if (refreshTimer.current !== null) window.clearTimeout(refreshTimer.current);
    };
  }, [refresh]);

  async function loadOlder() {
    if (!events || events.length === 0) return;
    try {
      const more = await api.listActivityEvents(serverId, events[events.length - 1].id, 50);
      setEvents((prev) => [...(prev ?? []), ...more]);
      setHasMore(more.length === 50);
    } catch (e) {
      setError(String(e));
    }
  }

  async function saveSettings(enabled: boolean, days: number) {
    setSavingSettings(true);
    try {
      await api.setActivityLogging(serverId, enabled, days);
      const fresh = await api.getActivityLogging(serverId);
      setSettings(fresh);
      setDraftDays(String(fresh.retention_days));
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setSavingSettings(false);
    }
  }

  function nameFor(pk: string): string {
    return memberByPk.get(pk) ?? pk.slice(0, 8) + "…";
  }

  return (
    <div>
      <div className="connect-section">
        <label className="settings-row">
          <input
            type="checkbox"
            checked={settings?.enabled ?? false}
            disabled={settings === null || savingSettings}
            onChange={(e) => { void saveSettings(e.target.checked, Number(draftDays) || 30); }}
          />
          Record who joins and leaves
        </label>
        <p className="settings-help">
          Connections, server joins and voice channels, visible to anyone with
          Manage Server. <strong>Direct-message calls are never recorded</strong>
          {" "}— who called whom is not the server's business, and turning
          this on does not change that. Turning it off stops new records; the
          ones already written age out on the window below.
        </p>

        <label className="connect-label" htmlFor="activity-retention">Keep records for</label>
        <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <input
            id="activity-retention"
            className="connect-input"
            type="number"
            min={1}
            max={730}
            style={{ width: 90 }}
            value={draftDays}
            disabled={settings === null || savingSettings}
            onChange={(e) => setDraftDays(e.target.value)}
          />
          <span className="settings-help" style={{ margin: 0 }}>days</span>
          <button
            className="xp-button"
            disabled={
              settings === null ||
              savingSettings ||
              String(settings.retention_days) === draftDays
            }
            onClick={() => { void saveSettings(settings?.enabled ?? true, Number(draftDays) || 30); }}
          >
            {savingSettings ? "Saving…" : "Apply"}
          </button>
        </div>
        <p className="settings-help">
          Between 1 and 730 days. There is no "forever" here on purpose: the
          moderation log keeps bans and kicks permanently, but a permanent record
          of when everyone is at their computer is a different kind of thing.
        </p>
      </div>

      {error && <div className="error-text">{error}</div>}

      {events === null && !error && <p className="settings-help">Loading…</p>}

      {events !== null && events.length === 0 && (
        <p className="settings-help">
          {settings?.enabled === false
            ? "Recording is off, so nothing new is being written."
            : "Nothing recorded yet."}
        </p>
      )}

      {events !== null && events.map((evt) => {
        const who = nameFor(publicKeyToString(evt.actor));
        const verb = ACTIVITY_VERBS[evt.action]
          ? ACTIVITY_VERBS[evt.action](evt.metadata)
          : evt.action;
        return (
          <div key={evt.id} style={rowStyle}>
            <strong>{who}</strong> {verb}{" "}
            <span className="settings-help" style={{ margin: 0 }}>
              · {relativeTime(evt.timestamp_ms)}
            </span>
          </div>
        );
      })}

      {hasMore && (
        <button className="xp-button" style={{ marginTop: 8 }} onClick={() => { void loadOlder(); }}>
          Load older
        </button>
      )}
    </div>
  );
}
