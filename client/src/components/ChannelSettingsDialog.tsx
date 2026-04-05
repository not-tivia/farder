import { useState } from "react";
import * as api from "../lib/tauri-bridge";
import type { ChannelInfo, RoleInfo } from "../lib/types";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";

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
  const [activeTab, setActiveTab] = useState<"general" | "permissions">("general");

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
      <div className="permission-role-name" style={{ color: role.color ? `#${role.color.toString(16).padStart(6, "0")}` : undefined }}>
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
