import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString, memberDisplayName } from "../lib/types";

interface Props {
  serverId: string;
  /** This account is the owner right now (only the owner decides who keeps a copy). */
  isOwner?: boolean;
}

/**
 * Backup addresses for this server, on THIS device: where to read a saved copy of the server when the
 * host's computer is off. A member who runs a replica (see docs/mesh-hosting-status.md) tells friends its
 * address; each friend types it here. The app tries the host first, then these, in order.
 */
export default function BackupCopyTab({ serverId, isOwner }: Props) {
  const server = useActiveServer();
  const [replicas, setReplicas] = useState<string[]>([]);
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.getFallbackAddresses(serverId).then((a) => setText(a.join("\n"))).catch((e) => setError(String(e)));
  }, [serverId]);

  useEffect(() => {
    if (isOwner) api.listReplicas(serverId).then(setReplicas).catch((e) => setError(String(e)));
  }, [serverId, isOwner]);

  async function toggleReplica(pk: string, on: boolean) {
    setBusy(true); setError(null);
    try { setReplicas(await api.setReplica(serverId, pk, on)); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  async function save() {
    setBusy(true); setError(null); setNote(null);
    try {
      await api.setFallbackAddresses(serverId, text.split(/\s+/).filter(Boolean));
      setNote("Saved. If the host does not answer, the app will try these.");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  return (
    <div style={{ padding: 8 }}>
      <div className="connect-section" style={{ fontSize: "var(--xp-font-size)" }}>
        If the computer that hosts this server is switched off, you can still <b>read</b> the history from a saved
        copy kept by another machine. Ask whoever runs one for its address (like 192.168.1.20:4435) and put it
        here, one per line. You can read, but sending waits until the host is back. This is only on this device.
      </div>
      <div className="connect-section">
        <label className="connect-label" htmlFor="backup-addresses">Backup addresses</label>
        <textarea id="backup-addresses" className="connect-input" rows={4} value={text} aria-label="Backup addresses"
          onChange={(e) => setText(e.target.value)} style={{ width: "100%", boxSizing: "border-box", fontFamily: "monospace" }} />
      </div>
      <button className="xp-button" disabled={busy} onClick={save}>Save</button>
      {isOwner && (
        <div className="connect-section" style={{ marginTop: 16 }}>
          <label className="connect-label">Who may keep a full copy of this server</label>
          <div style={{ fontSize: 11, marginBottom: 6 }}>
            A machine that keeps a copy sees <b>everything the host sees</b>, including channels others cannot open and
            invite codes. Only allow computers you trust as much as the host. The copy program is <code>farder-replica</code>.
          </div>
          {(server?.members ?? []).filter((m) => !m.is_bot).map((m) => {
            const pk = publicKeyToString(m.public_key);
            if (pk === (server?.ownerPublicKey ?? null)) return null;
            return (
              <label key={pk} style={{ display: "block" }}>
                <input type="checkbox" checked={replicas.includes(pk)} disabled={busy}
                  aria-label={`${memberDisplayName(m.display_name)} keeps a copy`}
                  onChange={(e) => toggleReplica(pk, e.target.checked)} />
                {" "}{memberDisplayName(m.display_name)}
              </label>
            );
          })}
        </div>
      )}
      {note && <div style={{ marginTop: 8 }}>{note}</div>}
      {error && <div className="error-text" style={{ marginTop: 8 }}>{error}</div>}
    </div>
  );
}
