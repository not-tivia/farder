import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useApp } from "../context/ServerContext";
import { parseInviteLink } from "../lib/invite";

type Step = "setup" | "choice" | "create-1" | "create-2" | "join";

const DEFAULT_TEMPLATES = [
  { id: "blank", name: "Blank", description: "Empty server — start from scratch" },
  { id: "friend-group", name: "Friends", description: "Casual hangout for a small group" },
  { id: "gaming-community", name: "Gaming", description: "Voice lobbies, LFG, and game channels" },
  { id: "organization", name: "Organization", description: "Teams, projects, and announcements" },
  { id: "public-community", name: "Community", description: "Public community with moderation" },
];


export default function ConnectDialog() {
  const { dispatch } = useApp();

  const [step, setStep] = useState<Step>("setup");
  const [displayName, setDisplayName] = useState("");
  const [savedName, setSavedName] = useState<string | null>(null);
  const [pubKey, setPubKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Create server state
  const [serverName, setServerName] = useState("");
  const [serverIcon, setServerIcon] = useState<string | null>(null);
  const [selectedTemplate, setSelectedTemplate] = useState("blank");
  const [privacy, setPrivacy] = useState("invite-only");

  // Join state
  const [inviteInput, setInviteInput] = useState("");

  useEffect(() => {
    async function init() {
      const [existingKey, existingName] = await Promise.allSettled([
        api.getPublicKey(),
        api.getDisplayName(),
      ]);

      const key = existingKey.status === "fulfilled" ? existingKey.value : null;
      const name = existingName.status === "fulfilled" ? existingName.value : null;

      if (key) setPubKey(key);

      if (key && name) {
        setSavedName(name);
        setStep("choice");
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
      const key = await api.getPublicKey();
      await api.setDisplayName(trimmed);
      if (key) setPubKey(key);
      setSavedName(trimmed);
      dispatch({ type: "SET_IDENTITY" });
      setStep("choice");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function handlePickIcon() {
    try {
      const path = await api.pickFile();
      if (path) setServerIcon(path);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleCreateServer() {
    const trimmedName = serverName.trim();
    if (!trimmedName) {
      setError("Please enter a server name.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const result = await api.createLocalServer(
        trimmedName,
        selectedTemplate,
        privacy,
        serverIcon ?? undefined,
      );
      const { address, ...connectPayload } = result;
      dispatch({ type: "SERVER_ADDED", serverId: address, payload: connectPayload });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: address });
      try {
        const members = await api.getMembers(address);
        dispatch({ type: "SET_MEMBERS", serverId: address, payload: members });
      } catch {}
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function handleJoin() {
    setLoading(true);
    setError(null);

    if (!pubKey) {
      const key = await api.getPublicKey();
      if (key) {
        setPubKey(key);
        dispatch({ type: "SET_IDENTITY" });
      }
    }

    try {
      const parsed = parseInviteLink(inviteInput);

      const address = parsed.address;
      if (!address) {
        setError("Include the server address in the link (e.g. farder://host:port/code).");
        setLoading(false);
        return;
      }

      const result = await api.connectServer(address, parsed.inviteCode, parsed.setupToken);
      dispatch({ type: "SERVER_ADDED", serverId: address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: address });
      try {
        const members = await api.getMembers(address);
        dispatch({ type: "SET_MEMBERS", serverId: address, payload: members });
      } catch {
        // non-fatal
      }
      try {
        const dms = await api.listDms(address);
        dispatch({ type: "SET_DMS", serverId: address, payload: dms });
      } catch {}
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

  // ── Setup step ──────────────────────────────────────────────
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

  // ── Choice step ─────────────────────────────────────────────
  if (step === "choice") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Welcome back, {savedName}!</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">What would you like to do?</div>
            </div>

            <div className="server-choice">
              <div
                className="server-choice-card"
                onClick={() => { setError(null); setStep("create-1"); }}
              >
                <div className="choice-icon">+</div>
                <div className="choice-title">Create a Server</div>
                <div className="choice-desc">Start your own community</div>
              </div>

              <div
                className="server-choice-card"
                onClick={() => { setError(null); setStep("join"); }}
              >
                <div className="choice-icon">&#x2192;</div>
                <div className="choice-title">Join a Server</div>
                <div className="choice-desc">Enter an invite link</div>
              </div>
            </div>

            <div className="connect-footer-links">
              <button className="connect-link" onClick={handleChangeName}>
                Change display name
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // ── Create step 1: Name + icon ──────────────────────────────
  if (step === "create-1") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Create a Server</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">Server Name</div>
              <input
                className="connect-input"
                type="text"
                placeholder="My Awesome Server"
                value={serverName}
                maxLength={64}
                onChange={(e) => setServerName(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter" && serverName.trim()) setStep("create-2"); }}
                autoFocus
              />
            </div>

            <div className="connect-section">
              <div className="connect-section-title">Server Icon (optional)</div>
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <button className="xp-button" onClick={handlePickIcon}>
                  Choose Image...
                </button>
                {serverIcon && (
                  <span style={{ fontSize: 11, color: "#666", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 180 }}>
                    {serverIcon.split(/[/\\]/).pop()}
                  </span>
                )}
              </div>
            </div>

            {error && <div className="error-text">{error}</div>}

            <div className="connect-actions">
              <button className="xp-button" onClick={() => { setError(null); setStep("choice"); }}>
                Back
              </button>
              <button
                className="xp-button"
                onClick={() => { setError(null); setStep("create-2"); }}
                disabled={!serverName.trim()}
              >
                Next
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // ── Create step 2: Template + privacy ───────────────────────
  if (step === "create-2") {
    return (
      <div className="connect-screen">
        <div className="connect-dialog">
          <div className="connect-dialog-titlebar">Create a Server</div>
          <div className="connect-dialog-body">
            <div className="connect-section">
              <div className="connect-section-title">Choose a Template</div>
              <div className="template-grid">
                {DEFAULT_TEMPLATES.map((t) => (
                  <div
                    key={t.id}
                    className={`template-card${selectedTemplate === t.id ? " selected" : ""}`}
                    onClick={() => setSelectedTemplate(t.id)}
                  >
                    <div className="tmpl-name">{t.name}</div>
                    <div className="tmpl-desc">{t.description}</div>
                  </div>
                ))}
              </div>
            </div>

            <div className="connect-section">
              <div className="connect-section-title">Privacy</div>
              <div className="privacy-options">
                <label className="privacy-option">
                  <input
                    type="radio"
                    name="privacy"
                    value="invite-only"
                    checked={privacy === "invite-only"}
                    onChange={() => setPrivacy("invite-only")}
                  />
                  Invite only
                </label>
                <label className="privacy-option">
                  <input
                    type="radio"
                    name="privacy"
                    value="open"
                    checked={privacy === "open"}
                    onChange={() => setPrivacy("open")}
                  />
                  Open
                </label>
              </div>
            </div>

            {error && <div className="error-text">{error}</div>}

            <div className="connect-actions">
              <button className="xp-button" onClick={() => { setError(null); setStep("create-1"); }}>
                Back
              </button>
              <button className="xp-button" onClick={handleCreateServer} disabled={loading}>
                {loading ? "Creating..." : "Create Server"}
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // ── Join step ───────────────────────────────────────────────
  return (
    <div className="connect-screen">
      <div className="connect-dialog">
        <div className="connect-dialog-titlebar">Join a Server</div>
        <div className="connect-dialog-body">
          <div className="connect-section">
            <div className="connect-section-title">Paste an invite link</div>
            <input
              className="connect-input"
              type="text"
              placeholder="farder://server/invite-code"
              value={inviteInput}
              onChange={(e) => setInviteInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") handleJoin(); }}
              autoFocus
            />
          </div>

          {error && <div className="error-text">{error}</div>}

          <div className="connect-actions">
            <button className="xp-button" onClick={() => { setError(null); setStep("choice"); }}>
              Back
            </button>
            <button className="xp-button" onClick={handleJoin} disabled={loading}>
              {loading ? "Connecting..." : "Join"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
