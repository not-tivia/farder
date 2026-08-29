import { useEffect, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import { E2eeConfirmDialog } from "./E2eeConfirmDialog";

const overlay: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.4)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2400,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 20,
  width: 420,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

/**
 * The devices of one member that can read this encrypted channel, with a revoke
 * action (sub-5b G3/G4, the owner half).
 *
 * Devices — not accounts — are what hold a channel's keys, so this is the only
 * view that answers "who can actually read this". The list is the group's ACTUAL
 * leaf set, not the roster: a device that claims membership but holds no leaf
 * cannot read, and a leaf whose holder is no longer entitled to it is exactly
 * the drift the channel refuses to send through.
 */
export function E2eeMemberDevices({
  serverId,
  logServerId,
  channelId,
  identity,
  memberName,
  onClose,
}: {
  serverId: string;
  logServerId: string;
  channelId: number;
  /** The member's `vk_…` identity string. */
  identity: string;
  memberName: string;
  onClose: () => void;
}) {
  const [devices, setDevices] = useState<api.ChannelLeaf[] | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .e2eeChannelLeaves(logServerId, channelId)
      .then((all) => {
        if (!cancelled) setDevices(all.filter((d) => d.identity === identity));
      })
      .catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [logServerId, channelId, identity, note]);

  async function revoke(device: string) {
    setBusy(true);
    setError(null);
    try {
      await api.revokeMemberDevice(serverId, logServerId, device);
      setNote("Device revoked. It can no longer read new messages anywhere on this server.");
      setConfirming(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div style={overlay} onClick={onClose}>
        <div style={card} onClick={(e) => e.stopPropagation()}>
          <h3 style={{ margin: "0 0 4px", fontSize: 14 }}>{memberName}&apos;s devices</h3>
          <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginBottom: 12, lineHeight: 1.5 }}>
            Devices, not accounts, hold a channel&apos;s keys. These are the ones that can read
            this channel right now.
          </div>

          {error && <div className="error-text" style={{ marginBottom: 8 }}>{error}</div>}
          {note && <div className="success-text" style={{ marginBottom: 8 }}>{note}</div>}

          {devices === null && <div style={{ fontSize: 12 }}>Loading…</div>}
          {devices?.length === 0 && (
            <div style={{ fontSize: 12, color: "var(--xp-text-muted)" }}>
              No devices of this member can read this channel.
            </div>
          )}
          {devices?.map((d) => (
            <div key={d.device} className="e2ee-device-row">
              <span className="e2ee-device-id" title={d.device}>
                {d.device.slice(0, 16)}…{d.is_own ? " (this device)" : ""}
              </span>
              {!d.is_own && (
                <button className="xp-button" onClick={() => setConfirming(d.device)} disabled={busy}>
                  Revoke
                </button>
              )}
            </div>
          ))}

          <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 14 }}>
            <button className="xp-button" onClick={onClose}>Close</button>
          </div>
        </div>
      </div>

      {confirming && (
        <E2eeConfirmDialog
          title="Revoke this device?"
          consequence={
            <>
              <strong>This cannot be undone.</strong> This device of {memberName} will no longer
              be able to read new messages in any encrypted channel on this server, and it can
              never be re-enabled.
              <br /><br />
              Their <em>account</em> is unaffected — their other devices keep working, and they
              can set that machine up again as a new device.
              <br /><br />
              Messages already saved on that device stay readable there. Revoking cannot reach
              back and take them away.
            </>
          }
          confirmLabel="Revoke device"
          busy={busy}
          error={error}
          onCancel={() => setConfirming(null)}
          onConfirm={() => void revoke(confirming)}
        />
      )}
    </>
  );
}
