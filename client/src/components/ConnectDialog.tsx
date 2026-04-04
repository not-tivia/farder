import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useServer } from "../context/ServerContext";

export default function ConnectDialog() {
  const { dispatch } = useServer();

  const [pubKey, setPubKey] = useState<string | null>(null);
  const [address, setAddress] = useState("");
  const [inviteCode, setInviteCode] = useState("");
  const [setupToken, setSetupToken] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    // Auto-load saved identity from disk on startup
    api.loadIdentity().then((k) => { if (k) setPubKey(k); }).catch(() => {});
  }, []);

  async function handleGenerate() {
    try {
      const key = await api.generateKeypair();
      setPubKey(key);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleConnect() {
    if (!address.trim()) {
      setError("Server address is required.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const result = await api.connectServer(
        address.trim(),
        inviteCode.trim() || undefined,
        setupToken.trim() || undefined,
      );
      dispatch({ type: "CONNECTED", payload: result });
      try {
        const members = await api.getMembers();
        dispatch({ type: "SET_MEMBERS", payload: members });
      } catch {
        // non-fatal
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="connect-screen">
      <div className="connect-dialog">
        <div className="connect-dialog-titlebar">Connect to Farder Server</div>
        <div className="connect-dialog-body">
          {/* Identity section */}
          <div className="connect-section">
            <div className="connect-section-title">Your Identity</div>
            {pubKey ? (
              <div className="connect-pubkey">{pubKey}</div>
            ) : (
              <span className="connect-label" style={{ color: "#999", fontStyle: "italic" }}>
                No identity generated yet.
              </span>
            )}
            <button className="xp-button" onClick={handleGenerate} style={{ alignSelf: "flex-start" }}>
              {pubKey ? "Regenerate Key" : "Generate Key"}
            </button>
          </div>

          {/* Server address */}
          <div className="connect-section">
            <div className="connect-section-title">Server</div>
            <label className="connect-label">Address</label>
            <input
              className="connect-input"
              type="text"
              placeholder="host:port"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleConnect(); }}
            />
            <label className="connect-label">Invite Code (optional)</label>
            <input
              className="connect-input"
              type="text"
              placeholder="Invite code"
              value={inviteCode}
              onChange={(e) => setInviteCode(e.target.value)}
            />
            <label className="connect-label">Setup Token (optional)</label>
            <input
              className="connect-input"
              type="text"
              placeholder="First-run setup token"
              value={setupToken}
              onChange={(e) => setSetupToken(e.target.value)}
            />
          </div>

          {error && <div className="error-text">{error}</div>}

          <div className="connect-actions">
            <button className="xp-button" onClick={handleConnect} disabled={loading}>
              {loading ? "Connecting…" : "Connect"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
