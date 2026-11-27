import { useCallback, useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import { GRANTABLE_POWERS, type Power } from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";

interface Props {
  serverId: string;
  logServerId: string;
}

const POWERS: Record<Power, { label: string; what: string }> = {
  invite: { label: "Invite", what: "Create invite links." },
  kick: { label: "Remove", what: "Remove members, and take down other people's attachments." },
  ban: { label: "Ban", what: "Ban and unban members." },
  garble: { label: "Garble", what: "Garble a member's next messages. Always labelled \"garbled by a moderator\" and recorded in the audit log." },
};

/**
 * Hand out and take back the powers the signed log knows about. Owner only: the
 * owner holds all of them and can revoke any; everything here is recorded in the
 * audit log. These are separate from role permissions (which still control the
 * rest of server settings).
 */
export default function PowersTab({ serverId, logServerId }: Props) {
  const server = useActiveServer();
  const [held, setHeld] = useState<Record<string, string[]>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(() => {
    api.getCapabilities(serverId)
      .then((hs) => {
        const m: Record<string, string[]> = {};
        for (const h of hs) m[h.member] = h.capabilities;
        setHeld(m);
        setLoaded(true);
      })
      .catch((e) => { setError(String(e)); setLoaded(true); });
  }, [serverId]);

  useEffect(() => {
    refresh();
    const onChange = (e: Event) => {
      const id = (e as CustomEvent<{ serverId: string }>).detail?.serverId;
      if (!id || id === serverId) refresh();
    };
    window.addEventListener("farder:capabilities-changed", onChange);
    return () => window.removeEventListener("farder:capabilities-changed", onChange);
  }, [serverId, refresh]);

  const ownerPk = server?.ownerPublicKey ?? null;
  const people = (server?.members ?? []).filter(
    (m) => !m.is_bot && publicKeyToString(m.public_key) !== ownerPk,
  );

  async function toggle(pk: string, power: Power, has: boolean) {
    const key = `${pk}:${power}`;
    setBusy(key); setError(null);
    try {
      if (has) await api.revokeCapability(serverId, logServerId, pk, power);
      else await api.grantCapability(serverId, logServerId, pk, power);
      refresh();
    } catch (e) { setError(String(e)); } finally { setBusy(null); }
  }

  return (
    <div style={{ padding: 8 }}>
      <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginBottom: 8 }}>
        You hold every power. Tick a box to give someone a power, untick to take it back.
        Every change is recorded in the Audit Log. Taking a power away from someone does not
        take back what they already handed on.
      </div>
      <table style={{ width: "100%", borderCollapse: "collapse", fontSize: "var(--xp-font-size)" }}>
        <thead>
          <tr style={{ textAlign: "left", borderBottom: "1px solid var(--xp-border)" }}>
            <th style={{ padding: "4px 6px" }}>Member</th>
            {GRANTABLE_POWERS.map((p) => (
              <th key={p} style={{ padding: "4px 6px" }} title={POWERS[p].what}>{POWERS[p].label}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {people.map((m) => {
            const pk = publicKeyToString(m.public_key);
            const mine = held[pk] ?? [];
            return (
              <tr key={pk} style={{ borderBottom: "1px solid var(--xp-border)" }}>
                <td style={{ padding: "4px 6px" }}>{m.display_name}</td>
                {GRANTABLE_POWERS.map((p) => {
                  const has = mine.includes(p);
                  return (
                    <td key={p} style={{ padding: "4px 6px" }}>
                      <input type="checkbox" checked={has} disabled={!loaded || busy !== null}
                        aria-label={`${POWERS[p].label} for ${m.display_name}`}
                        onChange={() => void toggle(pk, p, has)} />
                    </td>
                  );
                })}
              </tr>
            );
          })}
          {people.length === 0 && (
            <tr><td colSpan={1 + GRANTABLE_POWERS.length} style={{ padding: 8, color: "var(--xp-text-muted)" }}>
              Nobody else is on this server yet.
            </td></tr>
          )}
        </tbody>
      </table>
      <ul style={{ fontSize: 11, color: "var(--xp-text-muted)", marginTop: 10, paddingLeft: 18 }}>
        {GRANTABLE_POWERS.map((p) => <li key={p}><strong>{POWERS[p].label}</strong>: {POWERS[p].what}</li>)}
      </ul>
      {error && <div className="error-text" style={{ marginTop: 8 }}>{error}</div>}
    </div>
  );
}
