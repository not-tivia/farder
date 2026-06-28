import { useState } from "react";
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
