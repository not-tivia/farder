import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import type { ChannelInfo, RoleInfo, WebhookInfo, WebhookTokenResult } from "../lib/types";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";

// ---------------------------------------------------------------------------
// Relay webhook ingest base URL (v1: single known relay; change here when
// TLS / a reverse proxy is added or the relay host changes).
// ---------------------------------------------------------------------------
const RELAY_WEBHOOK_BASE = "http://45.77.70.199:8080";

interface Props {
  channel: ChannelInfo;
  onClose: () => void;
}

// Permission bit constants (must match server)
const VIEW_CHANNEL = 1 << 0;
const READ_MESSAGES = 1 << 1;
const SEND_MESSAGES = 1 << 2;

export default function ChannelSettingsDialog({ channel, onClose }: Props) {
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const [name, setName] = useState(channel.name);
  const [topic, setTopic] = useState(channel.topic ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);
  const [activeTab, setActiveTab] = useState<"general" | "permissions" | "webhooks">("general");

  // ── Webhooks tab state ────────────────────────────────────────────────────
  const [webhooks, setWebhooks] = useState<WebhookInfo[]>([]);
  const [webhooksLoaded, setWebhooksLoaded] = useState(false);
  const [webhookLoading, setWebhookLoading] = useState(false);
  const [webhookError, setWebhookError] = useState<string | null>(null);
  const [newWebhookName, setNewWebhookName] = useState("");
  const [creating, setCreating] = useState(false);
  /** Token result shown once after create or regenerate; null otherwise. */
  const [shownToken, setShownToken] = useState<WebhookTokenResult | null>(null);

  const roles = activeServer?.roles ?? [];

  async function handleSave() {
    if (!serverId) return;
    setSaving(true);
    setError(null);
    try {
      await api.updateChannel(serverId, channel.id, {
        name: name !== channel.name ? name : undefined,
        topic: topic !== (channel.topic ?? "") ? topic : undefined,
      });
      setSuccess(true);
      setTimeout(() => setSuccess(false), 2000);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  async function handleSetOverride(roleId: number, allow: number, deny: number) {
    if (!serverId) return;
    try {
      await api.setChannelOverride(serverId, channel.id, roleId, allow, deny);
    } catch (e) {
      setError(String(e));
    }
  }

  // ── Webhook handlers ──────────────────────────────────────────────────────
  useEffect(() => {
    if (activeTab !== "webhooks" || webhooksLoaded || !serverId) return;
    setWebhookLoading(true);
    setWebhookError(null);
    api.listWebhooks(serverId, channel.id)
      .then((list) => { setWebhooks(list); setWebhooksLoaded(true); })
      .catch((e) => setWebhookError(String(e)))
      .finally(() => setWebhookLoading(false));
  }, [activeTab, webhooksLoaded, serverId, channel.id]);

  async function handleCreateWebhook() {
    if (!serverId || !newWebhookName.trim()) return;
    setCreating(true);
    setWebhookError(null);
    setShownToken(null);
    try {
      const result = await api.createWebhook(serverId, channel.id, newWebhookName.trim());
      setWebhooks((prev) => [...prev, { id: result.id, channel_id: channel.id, name: newWebhookName.trim() }]);
      setShownToken(result);
      setNewWebhookName("");
    } catch (e) {
      setWebhookError(String(e));
    } finally {
      setCreating(false);
    }
  }

  async function handleDeleteWebhook(id: number) {
    if (!serverId) return;
    setWebhookError(null);
    try {
      await api.deleteWebhook(serverId, id);
      setWebhooks((prev) => prev.filter((w) => w.id !== id));
      if (shownToken?.id === id) setShownToken(null);
    } catch (e) {
      setWebhookError(String(e));
    }
  }

  async function handleRegenerateToken(id: number) {
    if (!serverId) return;
    setWebhookError(null);
    setShownToken(null);
    try {
      const result = await api.regenerateWebhookToken(serverId, id);
      setShownToken(result);
    } catch (e) {
      setWebhookError(String(e));
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()} style={{ minWidth: 400 }}>
        <div className="modal-titlebar">
          <span>Channel Settings — #{channel.name}</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          <div className="settings-tabs">
            <button className={`settings-tab${activeTab === "general" ? " active" : ""}`} onClick={() => setActiveTab("general")}>General</button>
            <button className={`settings-tab${activeTab === "permissions" ? " active" : ""}`} onClick={() => setActiveTab("permissions")}>Permissions</button>
            <button className={`settings-tab${activeTab === "webhooks" ? " active" : ""}`} onClick={() => setActiveTab("webhooks")}>Webhooks</button>
          </div>

          {activeTab === "general" && (
            <div className="settings-tab-content">
              <div className="connect-section">
                <label className="connect-label">Channel Name</label>
                <input className="connect-input" value={name} onChange={(e) => setName(e.target.value)} />
              </div>
              <div className="connect-section">
                <label className="connect-label">Topic</label>
                <input className="connect-input" value={topic} onChange={(e) => setTopic(e.target.value)} placeholder="Channel topic" />
              </div>
              {error && <div className="error-text">{error}</div>}
              {success && <div className="success-text">Saved!</div>}
              <div className="connect-actions">
                <button className="xp-button" onClick={handleSave} disabled={saving}>
                  {saving ? "Saving..." : "Save Changes"}
                </button>
              </div>
            </div>
          )}

          {activeTab === "permissions" && (
            <div className="settings-tab-content">
              <p style={{ fontSize: 11, color: "#666", marginBottom: 8 }}>
                Set permission overrides per role. Check to allow, uncheck to deny, leave empty to inherit.
              </p>
              {roles.map((role) => (
                <RolePermissionRow
                  key={role.id}
                  role={role}
                  channelId={channel.id}
                  onSet={handleSetOverride}
                />
              ))}
            </div>
          )}

          {activeTab === "webhooks" && (
            <div className="settings-tab-content">
              <div className="connect-section-title" style={{ marginBottom: 8 }}>Incoming Webhooks</div>
              <p style={{ fontSize: 11, color: "var(--xp-text-muted)", marginBottom: 10 }}>
                Webhooks let external services post messages to this channel. Copy the URL after creating one — the token is shown only once.
              </p>

              {webhookError && <div className="error-text" style={{ marginBottom: 8 }}>{webhookError}</div>}

              {/* Token shown once after create or regenerate */}
              {shownToken && (
                <div className="connect-section" style={{ marginBottom: 12 }}>
                  <label className="connect-label">
                    {shownToken.server_id_hex
                      ? "Webhook URL (copy now — token shown once)"
                      : "Token (copy now — shown once)"}
                  </label>
                  {shownToken.server_id_hex ? (
                    <input
                      className="connect-input"
                      readOnly
                      value={`${RELAY_WEBHOOK_BASE}/webhook/${shownToken.server_id_hex}/${shownToken.token}`}
                      onFocus={(e) => e.currentTarget.select()}
                    />
                  ) : (
                    <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginTop: 4 }}>
                      This server is not relay-connected — webhooks require the relay to receive inbound HTTP posts.
                    </div>
                  )}
                </div>
              )}

              {/* Existing webhooks list */}
              {webhookLoading && (
                <div style={{ color: "var(--xp-text-muted)", fontSize: 12, marginBottom: 8 }}>Loading...</div>
              )}
              {!webhookLoading && webhooks.length === 0 && webhooksLoaded && (
                <div style={{ color: "var(--xp-text-muted)", fontSize: 13, marginBottom: 8 }}>
                  No webhooks yet.
                </div>
              )}
              {webhooks.map((wh) => (
                <div key={wh.id} className="organizer-row">
                  <span className="organizer-name">{wh.name}</span>
                  <div className="organizer-actions">
                    <button
                      className="organizer-btn"
                      title="Regenerate token"
                      onClick={() => void handleRegenerateToken(wh.id)}
                    >
                      Regenerate
                    </button>
                    <button
                      className="organizer-btn organizer-delete"
                      title="Delete webhook"
                      onClick={() => void handleDeleteWebhook(wh.id)}
                    >
                      x
                    </button>
                  </div>
                </div>
              ))}

              {/* Create new webhook */}
              <div style={{ marginTop: 12, borderTop: "1px solid var(--xp-border)", paddingTop: 10 }}>
                <div className="connect-section-title" style={{ marginBottom: 6, fontSize: 12 }}>Create Webhook</div>
                <div style={{ display: "flex", gap: 6, alignItems: "flex-end" }}>
                  <div style={{ flex: 1 }}>
                    <label className="connect-label">Name</label>
                    <input
                      className="connect-input"
                      placeholder="e.g. CI alerts"
                      value={newWebhookName}
                      onChange={(e) => setNewWebhookName(e.target.value)}
                      onKeyDown={(e) => { if (e.key === "Enter") void handleCreateWebhook(); }}
                    />
                  </div>
                  <button
                    className="xp-button"
                    onClick={() => void handleCreateWebhook()}
                    disabled={creating || !newWebhookName.trim()}
                  >
                    {creating ? "Creating..." : "Create"}
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function RolePermissionRow({ role, channelId: _channelId, onSet }: { role: RoleInfo; channelId: number; onSet: (roleId: number, allow: number, deny: number) => void }) {
  const [viewAllow, setViewAllow] = useState(false);
  const [viewDeny, setViewDeny] = useState(false);
  const [sendAllow, setSendAllow] = useState(false);
  const [sendDeny, setSendDeny] = useState(false);

  function apply() {
    let allow = 0;
    let deny = 0;
    if (viewAllow) allow |= VIEW_CHANNEL | READ_MESSAGES;
    if (viewDeny) deny |= VIEW_CHANNEL | READ_MESSAGES;
    if (sendAllow) allow |= SEND_MESSAGES;
    if (sendDeny) deny |= SEND_MESSAGES;
    onSet(role.id, allow, deny);
  }

  return (
    <div className="permission-row">
      <div className="permission-role-name" style={{ color: role.color ?? undefined }}>
        {role.name}
      </div>
      <div className="permission-checks">
        <label className="permission-check">
          <span>View</span>
          <select value={viewAllow ? "allow" : viewDeny ? "deny" : "inherit"} onChange={(e) => {
            setViewAllow(e.target.value === "allow");
            setViewDeny(e.target.value === "deny");
          }}>
            <option value="inherit">Inherit</option>
            <option value="allow">Allow</option>
            <option value="deny">Deny</option>
          </select>
        </label>
        <label className="permission-check">
          <span>Send</span>
          <select value={sendAllow ? "allow" : sendDeny ? "deny" : "inherit"} onChange={(e) => {
            setSendAllow(e.target.value === "allow");
            setSendDeny(e.target.value === "deny");
          }}>
            <option value="inherit">Inherit</option>
            <option value="allow">Allow</option>
            <option value="deny">Deny</option>
          </select>
        </label>
        <button className="xp-button" onClick={apply} style={{ fontSize: 10, padding: "2px 6px" }}>Apply</button>
      </div>
    </div>
  );
}
