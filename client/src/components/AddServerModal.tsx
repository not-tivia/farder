import { useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

type Step = "choice" | "create-1" | "create-2" | "join";

const DEFAULT_TEMPLATES = [
  { id: "blank", name: "Blank", description: "Empty server — start from scratch" },
  { id: "friend-group", name: "Friends", description: "Casual hangout for a small group" },
  { id: "gaming-community", name: "Gaming", description: "Voice lobbies, LFG, and game channels" },
  { id: "organization", name: "Organization", description: "Teams, projects, and announcements" },
  { id: "public-community", name: "Community", description: "Public community with moderation" },
];

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
      const decoded = atob(joinMatch[1].replace(/-/g, "+").replace(/_/g, "/"));
      const slashIdx = decoded.indexOf("/");
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

export default function AddServerModal({ onClose }: { onClose: () => void }) {
  const { dispatch } = useApp();

  const [step, setStep] = useState<Step>("choice");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Create server state
  const [serverName, setServerName] = useState("");
  const [serverIcon, setServerIcon] = useState<string | null>(null);
  const [selectedTemplate, setSelectedTemplate] = useState("blank");
  const [privacy, setPrivacy] = useState("invite-only");

  // Join state
  const [inviteInput, setInviteInput] = useState("");

  async function handlePickIcon() {
    try {
      const path = await api.pickFile();
      if (path) setServerIcon(path);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleCreate() {
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
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function handleJoin() {
    setLoading(true);
    setError(null);
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
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  function titleFor(s: Step): string {
    if (s === "create-1" || s === "create-2") return "Create a Server";
    if (s === "join") return "Join a Server";
    return "Add Server";
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>{titleFor(step)}</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">

          {/* ── Choice ── */}
          {step === "choice" && (
            <>
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
            </>
          )}

          {/* ── Create step 1: Name + icon ── */}
          {step === "create-1" && (
            <>
              <div className="connect-section">
                <div className="connect-section-title">Server Name</div>
                <input
                  className="connect-input"
                  type="text"
                  placeholder="My Awesome Server"
                  value={serverName}
                  maxLength={64}
                  onChange={(e) => setServerName(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter" && serverName.trim()) { setError(null); setStep("create-2"); } }}
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
            </>
          )}

          {/* ── Create step 2: Template + privacy ── */}
          {step === "create-2" && (
            <>
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
                <button className="xp-button" onClick={handleCreate} disabled={loading}>
                  {loading ? "Creating..." : "Create Server"}
                </button>
              </div>
            </>
          )}

          {/* ── Join ── */}
          {step === "join" && (
            <>
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
            </>
          )}

        </div>
      </div>
    </div>
  );
}
