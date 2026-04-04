import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useServer } from "../context/ServerContext";

type Step = "setup" | "join";

export default function ConnectDialog() {
  const { dispatch } = useServer();

  const [step, setStep] = useState<Step>("setup");
  const [displayName, setDisplayName] = useState("");
  const [savedName, setSavedName] = useState<string | null>(null);
  const [address, setAddress] = useState("");
  const [token, setToken] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [pubKey, setPubKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // On mount: check for existing identity and saved settings.
  useEffect(() => {
    async function init() {
      const [existingKey, existingName, lastServer] = await Promise.allSettled([
        api.loadIdentity(),
        api.getDisplayName(),
        api.getLastServer(),
      ]);

      const key = existingKey.status === "fulfilled" ? existingKey.value : null;
      const name = existingName.status === "fulfilled" ? existingName.value : null;
      const server = lastServer.status === "fulfilled" ? lastServer.value : null;

      if (key) setPubKey(key);
      if (server) setAddress(server);

      if (key && name) {
        // Returning user — skip straight to join.
        setSavedName(name);
        setStep("join");
      }
      // Otherwise stay on setup step.
    }
    init().catch(() => {});
  }, []);

  // Step 1: first-time setup — generate keypair and save display name.
  async function handleContinue() {
    const trimmed = displayName.trim();
    if (!trimmed) {
      setError("Please enter a display name.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const key = await api.generateKeypair();
      await api.setDisplayName(trimmed);
      setPubKey(key);
      setSavedName(trimmed);
      setStep("join");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  // Step 2: connect to server.
  async function handleConnect() {
    if (!address.trim()) {
      setError("Server address is required.");
      return;
    }
    setLoading(true);
    setError(null);

    // If no keypair yet (shouldn't happen in normal flow), generate one.
    if (!pubKey) {
      try {
        const key = await api.generateKeypair();
        setPubKey(key);
      } catch (e) {
        setError(String(e));
        setLoading(false);
        return;
      }
    }

    try {
      const trimmedToken = token.trim();
      // The single token field could be either an invite code or a setup token.
      // Invite codes are typically short alphanumeric; setup tokens are hex.
      // We pass the same value to both fields and let the server sort it out —
      // actually the server expects exactly one. We detect by format:
      // setup tokens are 64-char hex, invite codes are shorter.
      const isSetupToken = /^[0-9a-f]{64}$/i.test(trimmedToken);
      const inviteCode = !isSetupToken && trimmedToken ? trimmedToken : undefined;
      const setupToken = isSetupToken ? trimmedToken : undefined;

      const result = await api.connectServer(
        address.trim(),
        inviteCode,
        setupToken,
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

  function handleChangeName() {
    setStep("setup");
    setDisplayName(savedName ?? "");
    setError(null);
  }

  if (step === "setup") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Welcome to Farder</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">What should we call you?</div>
              <input
                className="connect-input"
                type="text"
                placeholder="Display name"
                value={displayName}
                maxLength={32}
                onChange={(e) => setDisplayName(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") handleContinue(); }}
                autoFocus
              />
            </div>

            {error && <div className="error-text">{error}</div>}

            <div className="connect-actions">
              <button
                className="xp-button"
                onClick={handleContinue}
                disabled={loading}
              >
                {loading ? "Setting up..." : "Continue"}
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Step "join"
  return (
    <div className="connect-screen">
      <div className="connect-dialog">
        <div className="connect-dialog-titlebar">Join a Server</div>
        <div className="connect-dialog-body">
          <div className="connect-section">
            <div className="connect-section-title">Hi, {savedName}!</div>
          </div>

          <div className="connect-section">
            <label className="connect-label">Server Address</label>
            <input
              className="connect-input"
              type="text"
              placeholder="host:port"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleConnect(); }}
            />
            <label className="connect-label">Invite Code or Setup Token</label>
            <input
              className="connect-input"
              type="text"
              placeholder="Paste your invite code or setup token"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleConnect(); }}
            />
          </div>

          {error && <div className="error-text">{error}</div>}

          <div className="connect-actions">
            <button className="xp-button" onClick={handleConnect} disabled={loading}>
              {loading ? "Connecting..." : "Connect"}
            </button>
          </div>

          <div className="connect-footer-links">
            <button className="connect-link" onClick={handleChangeName}>
              Change display name
            </button>
            <span className="connect-link-sep"> | </span>
            <button
              className="connect-link"
              onClick={() => setShowAdvanced((v) => !v)}
            >
              {showAdvanced ? "Hide advanced" : "Advanced"}
            </button>
          </div>

          {showAdvanced && pubKey && (
            <div className="connect-section connect-advanced">
              <div className="connect-section-title">Public Key</div>
              <div className="connect-pubkey">{pubKey}</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
