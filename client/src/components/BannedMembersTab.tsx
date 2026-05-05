import { useEffect, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { BannedMember } from "../lib/types";

interface Props {
  serverId: string;
}

const row: CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  padding: "8px 0",
  borderBottom: "1px solid var(--xp-border, #ccc)",
  gap: 12,
};

export default function BannedMembersTab({ serverId }: Props) {
  const [entries, setEntries] = useState<BannedMember[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setLoading(true);
    setError(null);
    try {
      setEntries(await api.listBanned(serverId));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => { void refresh(); }, [serverId]);

  // Listen for the custom event broadcast by useServerEvents when a member is unbanned.
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent).detail as { serverId: string };
      if (detail.serverId === serverId) void refresh();
    };
    window.addEventListener("farder:banned-list-changed", handler);
    return () => window.removeEventListener("farder:banned-list-changed", handler);
  }, [serverId]);

  async function unban(entry: BannedMember) {
    if (!window.confirm(`Unban ${entry.display_name}? They'll be able to rejoin with this identity.`)) return;
    try {
      await api.unbanMember(serverId, entry.public_key);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div style={{ padding: 12 }}>
      <h3 style={{ marginTop: 0 }}>Banned Members</h3>
      {loading && <div>Loading…</div>}
      {error && <div style={{ color: "#a00" }}>{error}</div>}
      {!loading && entries.length === 0 && (
        <div style={{ color: "var(--xp-text-muted, #666)", fontSize: 11 }}>
          No banned members.
        </div>
      )}
      {entries.map((e) => (
        <div key={e.public_key} style={row}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: "bold" }}>{e.display_name}</div>
            <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", overflow: "hidden", textOverflow: "ellipsis" }}>
              {e.ban_reason ?? <em>(no reason)</em>}
            </div>
          </div>
          <button
            onClick={() => void unban(e)}
            style={{ font: "inherit", padding: "4px 10px" }}
          >
            Unban
          </button>
        </div>
      ))}
    </div>
  );
}
