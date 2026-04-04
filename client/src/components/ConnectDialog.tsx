import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useServer } from "../context/ServerContext";

type Step = "setup" | "join";

/** Parse a farder:// invite link into address + invite/setup token.
 *  Formats:
 *    farder://host:port/inviteCode
 *    farder://host:port/setup:hextoken
 *    host:port (bare address, no invite)
 *    raw invite code or setup token (used with saved server address)
 */
function parseInviteLink(input: string): {
  address?: string;
  inviteCode?: string;
  setupToken?: string;
} {
  const trimmed = input.trim();
  if (!trimmed) return {};

  // farder.gg/join/ENCODED format
  const joinMatch = trimmed.match(/(?:https?:\/\/)?farder\.gg\/join\/([A-Za-z0-9_-]+)/);
  if (joinMatch) {
    try {
      const decoded = atob(joinMatch[1].replace(/-/g, '+').replace(/_/g, '/'));
      const slashIdx = decoded.indexOf('/');
      if (slashIdx > 0) {
        const address = decoded.substring(0, slashIdx);
        const token = decoded.substring(slashIdx + 1);
        if (token.startsWith("setup:")) {
          return { address, setupToken: token.slice(6) };
        }
        return { address, inviteCode: token };
      }
    } catch {}
  }

  // farder:// protocol link
  const farderMatch = trimmed.match(/^farder:\/\/([^/]+)\/(.+)$/i);
  if (farderMatch) {
    const address = farderMatch[1];
    const token = farderMatch[2];
    if (token.startsWith("setup:")) {
      return { address, setupToken: token.slice(6) };
    }
    return { address, inviteCode: token };
  }

  // host:port/code format (without farder://)
  const slashMatch = trimmed.match(/^([^/]+:\d+)\/(.+)$/);
  if (slashMatch) {
    const address = slashMatch[1];
    const token = slashMatch[2];
    if (token.startsWith("setup:")) {
      return { address, setupToken: token.slice(6) };
    }
    return { address, inviteCode: token };
  }

  // 64-char hex = standalone setup token
  if (/^[0-9a-f]{64}$/i.test(trimmed)) {
    return { setupToken: trimmed };
  }

  // host:port only (no invite)
  if (/^.+:\d+$/.test(trimmed)) {
    return { address: trimmed };
  }

  // Short string = invite code
  return { inviteCode: trimmed };
}

export default function ConnectDialog() {
  const { dispatch } = useServer();

  const [step, setStep] = useState<Step>("setup");
  const [displayName, setDisplayName] = useState("");
  const [savedName, setSavedName] = useState<string | null>(null);
  const [inviteInput, setInviteInput] = useState("");
  const [savedAddress, setSavedAddress] = useState<string | null>(null);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [manualAddress, setManualAddress] = useState("");
  const [pubKey, setPubKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [autoConnecting, setAutoConnecting] = useState(false);

  async function autoConnect(server: string, key: string) {
    setAutoConnecting(true);
    setError(null);
    try {
      // Ensure identity is loaded in Tauri state before connecting
      await api.loadIdentity();
      const result = await api.connectServer(server);
      dispatch({ type: "CONNECTED", payload: result });
      try {
        const members = await api.getMembers();
        dispatch({ type: "SET_MEMBERS", payload: members });
      } catch {
        // non-fatal
      }
    } catch (e) {
      setError(`Could not reconnect: ${String(e)}`);
      setPubKey(key);
      setAutoConnecting(false);
    }
  }

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
      if (server) {
        setSavedAddress(server);
        setManualAddress(server);
      }

      if (key && name) {
        setSavedName(name);
        setStep("join");
        if (server) {
          autoConnect(server, key);
        }
      }
    }
    init().catch(() => {});
  }, []);

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

  async function handleConnect() {
    setLoading(true);
    setError(null);

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
      const parsed = parseInviteLink(inviteInput);

      // Use parsed address, or manual override, or saved address
      const address = parsed.address || manualAddress.trim() || savedAddress;
      if (!address) {
        // No address found anywhere — need user to provide one
        setShowAdvanced(true);
        setError("Include the server address in the link (e.g. farder://host:port/code) or enter it below.");
        setLoading(false);
        return;
      }

      const result = await api.connectServer(
        address,
        parsed.inviteCode,
        parsed.setupToken,
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
              <button className="xp-button" onClick={handleContinue} disabled={loading}>
                {loading ? "Setting up..." : "Continue"}
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (autoConnecting) {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Farder</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">Reconnecting...</div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="connect-screen">
      <div className="connect-dialog">
        <div className="connect-dialog-titlebar">Join a Server</div>
        <div className="connect-dialog-body">
          <div className="connect-section">
            <div className="connect-section-title">Hi, {savedName}!</div>
          </div>

          <div className="connect-section">
            <label className="connect-label">Paste an invite link</label>
            <input
              className="connect-input"
              type="text"
              placeholder="farder://server/invite-code"
              value={inviteInput}
              onChange={(e) => setInviteInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleConnect(); }}
              autoFocus
            />
            <div className="connect-hint">
              {savedAddress && !inviteInput.trim()
                ? `Will reconnect to ${savedAddress}`
                : ""}
            </div>
          </div>

          {error && <div className="error-text">{error}</div>}

          <div className="connect-actions">
            <button className="xp-button" onClick={handleConnect} disabled={loading}>
              {loading ? "Connecting..." : "Join"}
            </button>
          </div>

          <div className="connect-footer-links">
            <button className="connect-link" onClick={handleChangeName}>
              Change display name
            </button>
            <span className="connect-link-sep"> | </span>
            <button className="connect-link" onClick={() => setShowAdvanced((v) => !v)}>
              {showAdvanced ? "Hide advanced" : "Advanced"}
            </button>
          </div>

          {showAdvanced && (
            <div className="connect-section connect-advanced">
              <label className="connect-label">Server Address (manual)</label>
              <input
                className="connect-input"
                type="text"
                placeholder="host:port"
                value={manualAddress}
                onChange={(e) => setManualAddress(e.target.value)}
              />
              {pubKey && (
                <>
                  <div className="connect-section-title" style={{ marginTop: 8 }}>Public Key</div>
                  <div className="connect-pubkey">{pubKey}</div>
                </>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
