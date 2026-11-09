import { useEffect, useState } from "react";
import { emit } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import * as api from "../lib/tauri-bridge";
import { BUDDY_READY_EVENT, BUDDY_STATE_EVENT, type BuddyEntry, type BuddyState } from "../lib/buddy";

// The view rendered inside the buddy list window (see src-tauri/src/buddy.rs): your
// people and channels in one narrow column, like MSN Messenger. It REUSES the
// channel list's classes so it looks like the sidebar in every theme. The main
// window decides what is in the list (`buddy:state`); this page only draws it and
// asks for a chat window to be opened.

function badge(n: number): string {
  return n >= 99 ? "99+" : String(n);
}

export default function BuddyList() {
  const [entries, setEntries] = useState<BuddyEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    getCurrentWebviewWindow().listen<BuddyState>(BUDDY_STATE_EVENT, async (e) => {
      try {
        // A theme change made while the app runs should show here too.
        const { id: themeId, css } = await api.getActiveTheme();
        const style = document.getElementById("active-theme");
        if (style && document.documentElement.dataset.theme !== themeId) {
          style.textContent = css;
          document.documentElement.dataset.theme = themeId;
        }
      } catch { /* keep the current look */ }
      setEntries(e.payload?.entries ?? []);
    }).then((u) => { if (cancelled) u(); else unlisten = u; });
    void emit(BUDDY_READY_EVENT, {});
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  function open(en: BuddyEntry) {
    setError(null);
    const title = en.kind === "person" ? en.label : `#${en.label}`;
    api.chatOpen(en.server_id, en.channel_id, title).catch((e) => setError(String(e)));
  }

  const people = (entries ?? []).filter((e) => e.kind === "person");
  const channels = (entries ?? []).filter((e) => e.kind === "channel");
  const servers = [...new Set(channels.map((c) => c.server_name))];
  const unreadTotal = (entries ?? []).reduce((n, e) => n + e.unread, 0);

  return (
    <div className="channel-sidebar buddy-list" style={{ width: "100%", minWidth: 0, height: "100vh", boxSizing: "border-box" }}>
      <div className="server-header">
        <div className="server-name" style={{ flex: 1, minWidth: 0 }}>Farder{unreadTotal > 0 ? ` (${badge(unreadTotal)})` : ""}</div>
        <button className="server-invite-btn" onClick={() => api.stripOpen(null, null).catch(() => {})} title="Open the Farder window">&#9632;</button>
      </div>
      <div className="channel-list" style={{ flex: 1, overflowY: "auto" }}>
        {entries === null && <div className="channel-category">LOADING...</div>}
        {entries !== null && entries.length === 0 && (
          <div className="channel-category">NOTHING TO SHOW YET</div>
        )}
        {people.length > 0 && <div className="channel-category">PEOPLE ({people.length})</div>}
        {people.map((p) => (
          <BuddyRow key={`${p.server_id}:${p.channel_id}`} entry={p} onOpen={open} />
        ))}
        {servers.map((name) => (
          <div key={name}>
            <div className="channel-category">{name.toUpperCase()}</div>
            {channels.filter((c) => c.server_name === name).map((c) => (
              <BuddyRow key={`${c.server_id}:${c.channel_id}`} entry={c} onOpen={open} />
            ))}
          </div>
        ))}
      </div>
      {error && <div className="error-text" style={{ padding: 6 }}>{error}</div>}
    </div>
  );
}

function BuddyRow({ entry, onOpen }: { entry: BuddyEntry; onOpen: (e: BuddyEntry) => void }) {
  const person = entry.kind === "person";
  return (
    <div
      className={`channel-item${person ? " dm-item" : ""}${entry.unread > 0 ? " unread" : ""}`}
      role="button"
      tabIndex={0}
      title="Double-click to open a chat window"
      onDoubleClick={() => onOpen(entry)}
      onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); onOpen(entry); } }}
    >
      {person
        ? <span className="dm-avatar-mini">{(entry.label || "?").charAt(0).toUpperCase()}</span>
        : <span className="channel-prefix">{entry.encrypted ? "🔒" : "#"}</span>}
      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{entry.label}</span>
      {entry.unread > 0 && <span className="buddy-unread">{badge(entry.unread)}</span>}
    </div>
  );
}
