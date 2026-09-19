import { useCallback, useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import { useApp } from "../context/ServerContext";

/**
 * The moderator queue.
 *
 * In an encrypted channel this is where moderation begins and, mostly, where it
 * lives: the server cannot search for anything, so the only things it knows are
 * wrong are the ones somebody pointed at.
 *
 * Two deliberate choices here. Resolved reports STAY in the list — "looked at
 * it, did nothing" is an outcome a moderation record has to be able to show, and
 * a queue that empties on dismissal cannot tell that apart from never having
 * been read. And the attached copy, when there is one, is shown behind a click:
 * it is a member's decrypted message, not queue furniture, and it should take a
 * deliberate act to read.
 */
export default function ReportsTab({ serverId }: { serverId: string }) {
  const { state } = useApp();
  const [reports, setReports] = useState<api.ReportInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [shown, setShown] = useState<Set<number>>(new Set());

  const channelName = (id: number) =>
    state.servers[serverId]?.channels.find((c) => c.id === id)?.name ?? `channel ${id}`;

  const refresh = useCallback(async () => {
    try {
      setReports(await api.listReports(serverId));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, [serverId]);

  useEffect(() => { void refresh(); }, [refresh]);

  async function resolve(id: number, outcome: string) {
    setBusy(id);
    try {
      await api.resolveReport(serverId, id, outcome);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  const open = (reports ?? []).filter((r) => !r.outcome);
  const closed = (reports ?? []).filter((r) => r.outcome);

  function row(r: api.ReportInfo) {
    const isShown = shown.has(r.id);
    return (
      <div key={r.id} className="connect-section" style={{ marginBottom: 10 }}>
        <div className="connect-section-title">
          {r.author_name ?? (r.author ? `${r.author.slice(0, 12)}…` : "unknown author")}
          {" in #"}{channelName(r.channel_id)}
        </div>
        <div style={{ fontSize: 12, marginBottom: 4 }}>{r.reason}</div>
        <div className="settings-help">
          reported by {r.reporter_name ?? `${r.reporter.slice(0, 12)}…`}
          {" · "}{new Date(r.created_at * 1000).toLocaleString()}
          {r.outcome && <> · <strong>{r.outcome}</strong></>}
        </div>

        {r.evidence !== null && (
          isShown ? (
            <div className="invite-code-display">
              <div className="invite-code-label">Copy attached by the reporter</div>
              <div className="invite-code-value">{r.evidence}</div>
            </div>
          ) : (
            <button
              className="link-embed-chip"
              style={{ marginTop: 6 }}
              onClick={() => setShown((s) => new Set(s).add(r.id))}
            >
              Show the attached copy
            </button>
          )
        )}
        {r.evidence === null && (
          <p className="settings-help">
            No copy attached — the reporter described it rather than handing over
            the text. You can still delete the message without reading it.
          </p>
        )}

        {!r.outcome && (
          <div style={{ display: "flex", gap: 6, marginTop: 8, flexWrap: "wrap" }}>
            <button className="xp-button" disabled={busy === r.id} onClick={() => { void resolve(r.id, "actioned"); }}>
              Mark actioned
            </button>
            <button className="xp-button" disabled={busy === r.id} onClick={() => { void resolve(r.id, "no action"); }}>
              No action needed
            </button>
          </div>
        )}
      </div>
    );
  }

  return (
    <div>
      {error && <div className="error-text">{error}</div>}
      {reports === null && !error && <p className="settings-help">Loading…</p>}

      {reports !== null && reports.length === 0 && (
        <p className="settings-help">
          Nothing reported. In an encrypted channel this queue is the only way
          anything reaches you — the server cannot look for itself.
        </p>
      )}

      {open.length > 0 && (
        <>
          <div className="connect-section-title">Open ({open.length})</div>
          {open.map(row)}
        </>
      )}

      {closed.length > 0 && (
        <>
          <div className="connect-section-title" style={{ marginTop: 14 }}>Handled</div>
          {closed.map(row)}
        </>
      )}
    </div>
  );
}
