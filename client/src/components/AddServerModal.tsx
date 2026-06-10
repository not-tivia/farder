import { useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { parseInviteLink } from "../lib/invite";

type Step = "choice" | "create-1" | "create-2" | "join";

const DEFAULT_TEMPLATES = [
  { id: "blank", name: "Blank", description: "Empty server — start from scratch" },
  { id: "friend-group", name: "Friends", description: "Casual hangout for a small group" },
  { id: "gaming-community", name: "Gaming", description: "Voice lobbies, LFG, and game channels" },
  { id: "organization", name: "Organization", description: "Teams, projects, and announcements" },
  { id: "public-community", name: "Community", description: "Public community with moderation" },
];


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

  // Relay choice state
  const [relayMode, setRelayMode] = useState<"farder" | "selfhost" | "direct">("farder");
  const [relayAddr, setRelayAddr] = useState("");
  const [relayFp, setRelayFp] = useState("");
  const [showRelayInfo, setShowRelayInfo] = useState(false);

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
    if (relayMode === "selfhost" && (!relayAddr.trim() || !relayFp.trim())) {
      setError("Enter the relay address and fingerprint.");
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
        relayMode,
        relayMode === "selfhost" ? relayAddr.trim() : undefined,
        relayMode === "selfhost" ? relayFp.trim() : undefined,
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

              <div className="connect-section">
                <div className="connect-section-title">How will people reach your server?</div>
                <div className="relay-cards">

                  {/* Farder relay card */}
                  <div
                    className={`relay-card${relayMode === "farder" ? " selected" : ""}`}
                    onClick={() => setRelayMode("farder")}
                  >
                    <div className="relay-card-header">
                      <input
                        type="radio"
                        name="relayMode"
                        checked={relayMode === "farder"}
                        onChange={() => setRelayMode("farder")}
                        onClick={(e) => e.stopPropagation()}
                      />
                      <span className="relay-card-title">Use the Farder relay</span>
                      <span className="relay-badge recommended">Recommended</span>
                    </div>
                    <div className="relay-card-desc">
                      Members&apos; IPs and yours stay hidden, and it works even behind a home router.
                    </div>
                  </div>

                  {/* Self-host card */}
                  <div
                    className={`relay-card${relayMode === "selfhost" ? " selected" : ""}`}
                    onClick={() => setRelayMode("selfhost")}
                  >
                    <div className="relay-card-header">
                      <input
                        type="radio"
                        name="relayMode"
                        checked={relayMode === "selfhost"}
                        onChange={() => setRelayMode("selfhost")}
                        onClick={(e) => e.stopPropagation()}
                      />
                      <span className="relay-card-title">Self-host your own relay</span>
                      <span className="relay-badge advanced">Advanced</span>
                    </div>
                    <div className="relay-card-desc">
                      Point at a relay you run yourself.
                    </div>
                    {relayMode === "selfhost" && (
                      <div className="relay-selfhost-fields">
                        <input
                          className="connect-input"
                          placeholder="Relay address (host:port)"
                          value={relayAddr}
                          onChange={(e) => setRelayAddr(e.target.value)}
                          onClick={(e) => e.stopPropagation()}
                        />
                        <input
                          className="connect-input"
                          placeholder="Cert fingerprint (64 hex characters)"
                          value={relayFp}
                          onChange={(e) => setRelayFp(e.target.value)}
                          onClick={(e) => e.stopPropagation()}
                        />
                      </div>
                    )}
                  </div>

                  {/* Direct card */}
                  <div
                    className={`relay-card${relayMode === "direct" ? " selected" : ""}`}
                    onClick={() => setRelayMode("direct")}
                  >
                    <div className="relay-card-header">
                      <input
                        type="radio"
                        name="relayMode"
                        checked={relayMode === "direct"}
                        onChange={() => setRelayMode("direct")}
                        onClick={(e) => e.stopPropagation()}
                      />
                      <span className="relay-card-title">Direct &mdash; same network only</span>
                      <span className="relay-badge advanced">Advanced</span>
                    </div>
                    <div className="relay-card-desc">
                      Connects straight to your machine. Only reachable on your own network or with
                      port-forwarding, and your IP is visible.
                    </div>
                  </div>

                </div>

                <button type="button" className="learn-more-toggle" onClick={() => setShowRelayInfo(!showRelayInfo)}>
                  {showRelayInfo ? "Hide details ^" : "Learn more v"}
                </button>
                {showRelayInfo && (
                  <div className="learn-more-body">
                    <p>A relay is a neutral middle server. Because you and your members connect <em>through</em> it instead of directly to each other, neither side learns the other&apos;s IP address &mdash; and your server stays reachable even behind a home router.</p>
                    <p>For this to protect you, the relay must be run by a neutral party (a relay run by the server&apos;s own host can&apos;t hide IPs from that host). The Farder relay is that neutral party.</p>
                    <p><strong>One honest caveat:</strong> today a relay&apos;s operator can technically read a community&apos;s messages (they aren&apos;t yet end-to-end encrypted between members and the server). Your direct messages and voice are always end-to-end encrypted regardless. Removing even that is on the roadmap.</p>
                  </div>
                )}
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
