import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";

interface Props {
  onClose: () => void;
}

export default function InviteDialog({ onClose }: Props) {
  const serverId = useActiveServerId();
  const activeServer = useActiveServer();
  const [link, setLink] = useState<string | null>(null);
  const [maxUses, setMaxUses] = useState<number | undefined>(undefined);
  const [requiresApproval, setRequiresApproval] = useState(false);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Guest links: a browser join for people who do not want the app.
  const [ownPk, setOwnPk] = useState<string | null>(null);
  const [guestOn, setGuestOn] = useState<boolean | null>(null);
  const [guestName, setGuestName] = useState(() => (activeServer?.serverName ?? "").toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, ""));
  const [guestSecs, setGuestSecs] = useState(24 * 3600);
  const [guestLink, setGuestLink] = useState<string | null>(null);
  const [guestBusy, setGuestBusy] = useState(false);
  const [guestError, setGuestError] = useState<string | null>(null);
  const [guestCopied, setGuestCopied] = useState(false);
  const isOwner = !!ownPk && ownPk === (activeServer?.ownerPublicKey ?? null);

  useEffect(() => {
    api.getPublicKey().then(setOwnPk).catch(() => {});
    if (serverId) api.getGuestLinks(serverId).then(setGuestOn).catch(() => setGuestOn(null));
  }, [serverId]);

  async function toggleGuest(on: boolean) {
    if (!serverId) return;
    setGuestBusy(true); setGuestError(null);
    try { setGuestOn(await api.setGuestLinks(serverId, on)); setGuestLink(null); }
    catch (e) { setGuestError(String(e)); }
    finally { setGuestBusy(false); }
  }

  async function makeGuestLink() {
    if (!serverId) return;
    setGuestBusy(true); setGuestError(null); setGuestCopied(false);
    try {
      const r = await api.createGuestInvite(serverId, activeServer?.logServerId ?? null, guestName, guestSecs, maxUses);
      setGuestLink(r.link);
    } catch (e) { setGuestError(String(e)); }
    finally { setGuestBusy(false); }
  }

  async function copyGuest() {
    if (!guestLink) return;
    try { await navigator.clipboard.writeText(guestLink); setGuestCopied(true); setTimeout(() => setGuestCopied(false), 2000); } catch { /* the field is selectable */ }
  }

  async function handleCreate() {
    if (!serverId) return;
    setLoading(true);
    setError(null);
    try {
      const logServerId = activeServer?.logServerId ?? null;
      const result = await api.createInvite(serverId, logServerId, maxUses, requiresApproval);
      setLink(result.link);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function handleCopy() {
    if (!link) return;
    try {
      await navigator.clipboard.writeText(link);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // fallback: select text
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Create Invite</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          {!link ? (
            <>
              <div className="connect-section">
                <label className="connect-label">Max Uses</label>
                <select
                  className="connect-input"
                  value={maxUses ?? ""}
                  onChange={(e) => setMaxUses(e.target.value ? Number(e.target.value) : undefined)}
                >
                  <option value="">Unlimited</option>
                  <option value="1">1 use</option>
                  <option value="5">5 uses</option>
                  <option value="10">10 uses</option>
                  <option value="25">25 uses</option>
                </select>
              </div>
              <div className="connect-section">
                <label className="connect-label">
                  <input
                    type="checkbox"
                    checked={requiresApproval}
                    onChange={(e) => setRequiresApproval(e.target.checked)}
                  />
                  {" "}Require approval to join
                </label>
              </div>
              {error && <div className="error-text">{error}</div>}
              <div className="connect-actions">
                <button className="xp-button" onClick={handleCreate} disabled={loading || !serverId}>
                  {loading ? "Creating..." : "Create Invite Link"}
                </button>
              </div>
              <details className="connect-section" style={{ marginTop: 10 }}>
                <summary>Guest link (for friends without the app: opens in a browser)</summary>
                <div style={{ marginTop: 6 }}>
                  <div style={{ fontSize: 11, marginBottom: 6 }}>
                    A guest can read and write in the normal text channels only, with no voice, files, encrypted
                    channels or inviting. The link stops working after the time you pick. The browser gateway can
                    see the messages in those channels (like a relay), so do not share secrets there.
                  </div>
                  {guestOn === null && <div style={{ fontSize: 11 }}>Guest links are not available on this server.</div>}
                  {guestOn === false && (isOwner ? (
                    <label className="connect-label">
                      <input type="checkbox" checked={false} disabled={guestBusy} onChange={() => toggleGuest(true)} />
                      {" "}Allow guest links on this server
                    </label>
                  ) : <div style={{ fontSize: 11 }}>The owner has not allowed guest links on this server.</div>)}
                  {guestOn === true && (
                    <>
                      {isOwner && (
                        <label className="connect-label">
                          <input type="checkbox" checked disabled={guestBusy} onChange={() => toggleGuest(false)} />
                          {" "}Allow guest links on this server
                        </label>
                      )}
                      <div className="connect-section">
                        <label className="connect-label">Web name (what your host called this server on the gateway)</label>
                        <input className="connect-input" aria-label="Web name" value={guestName} onChange={(e) => setGuestName(e.target.value)} />
                      </div>
                      <div className="connect-section">
                        <label className="connect-label">Link works for</label>
                        <select className="connect-input" aria-label="Guest link duration" value={guestSecs} onChange={(e) => setGuestSecs(Number(e.target.value))}>
                          <option value={3600}>1 hour</option>
                          <option value={6 * 3600}>6 hours</option>
                          <option value={24 * 3600}>1 day</option>
                          <option value={7 * 24 * 3600}>7 days</option>
                        </select>
                      </div>
                      <button className="xp-button" disabled={guestBusy || !guestName.trim()} onClick={makeGuestLink}>
                        {guestBusy ? "Working..." : "Make a guest link"}
                      </button>
                      {guestLink && (
                        <div className="invite-link-display" style={{ marginTop: 8 }}>
                          <input className="connect-input" aria-label="Guest link" value={guestLink} readOnly onClick={(e) => (e.target as HTMLInputElement).select()} />
                          <button className="xp-button" onClick={copyGuest}>{guestCopied ? "Copied!" : "Copy"}</button>
                        </div>
                      )}
                    </>
                  )}
                  {guestError && <div className="error-text">{guestError}</div>}
                </div>
              </details>
            </>
          ) : (
            <>
              <div className="connect-section">
                <label className="connect-label">Share this link with your friends:</label>
                <div className="invite-link-display">
                  <input className="connect-input" value={link} readOnly onClick={(e) => (e.target as HTMLInputElement).select()} />
                  <button className="xp-button" onClick={handleCopy}>
                    {copied ? "Copied!" : "Copy"}
                  </button>
                </div>
              </div>
              <div className="connect-actions">
                <button className="xp-button" onClick={() => { setLink(null); setCopied(false); }}>
                  Create Another
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
