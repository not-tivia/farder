import { useState, useEffect } from "react";
import type { MemberInfo, RoleInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { useApp } from "../context/ServerContext";

interface Props {
  member: MemberInfo;
  roles: RoleInfo[];
  position: { x: number; y: number };
  onClose: () => void;
  isSelf?: boolean;
  serverId: string;
}

export default function UserProfilePopup({ member, roles, position, onClose, isSelf, serverId }: Props) {
  const { dispatch } = useApp();
  const pkStr = publicKeyToString(member.public_key);
  const memberRoles = roles.filter(r => member.role_ids.includes(r.id) && r.name !== "@everyone");
  const joinDate = new Date(member.joined_at * 1000).toLocaleDateString([], {
    year: "numeric", month: "short", day: "numeric"
  });
  const initial = member.display_name.charAt(0).toUpperCase();

  const defaultBannerColor = `hsl(${Math.abs(pkStr.split("").reduce((a, c) => a + c.charCodeAt(0), 0)) % 360}, 50%, 40%)`;

  const [bio, setBio] = useState<string | null>(null);
  const [bannerColor, setBannerColor] = useState(defaultBannerColor);
  const [editingBio, setEditingBio] = useState(false);
  const [bioInput, setBioInput] = useState("");

  useEffect(() => {
    if (isSelf) {
      api.getBio().then(b => { if (b) setBio(b); });
      api.getProfileColor().then(c => { if (c) setBannerColor(c); });
    }
  }, [isSelf]);

  async function saveBio() {
    const trimmed = bioInput.trim();
    await api.setBio(trimmed);
    setBio(trimmed || null);
    setEditingBio(false);
  }

  const style: React.CSSProperties = {
    position: "fixed",
    top: Math.min(position.y, window.innerHeight - 380),
    left: Math.min(position.x, window.innerWidth - 300),
    zIndex: 1000,
  };

  return (
    <>
      <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={onClose} />
      <div className="profile-card" style={style}>
        {/* Banner */}
        <div className="profile-card-banner" style={{ background: bannerColor }} />

        {/* Avatar */}
        <div className="profile-card-avatar-row">
          <div className="profile-card-avatar" style={{ background: bannerColor }}>
            {initial}
          </div>
        </div>

        {/* Info */}
        <div className="profile-card-body">
          <div className="profile-card-name">{member.display_name}</div>
          <div className="profile-card-id">{pkStr.slice(0, 18)}...</div>

          <div className="profile-card-divider" />

          {/* Bio */}
          {(bio || isSelf) && (
            <div className="profile-card-section">
              <div className="profile-card-label">ABOUT ME</div>
              {editingBio ? (
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <textarea
                    className="profile-card-bio-input"
                    value={bioInput}
                    onChange={(e) => setBioInput(e.target.value)}
                    maxLength={190}
                    rows={3}
                    autoFocus
                    placeholder="Tell people about yourself..."
                  />
                  <div style={{ display: "flex", gap: 4, justifyContent: "flex-end" }}>
                    <button className="xp-button" onClick={() => setEditingBio(false)} style={{ fontSize: 10, padding: "2px 8px" }}>Cancel</button>
                    <button className="xp-button" onClick={saveBio} style={{ fontSize: 10, padding: "2px 8px" }}>Save</button>
                  </div>
                </div>
              ) : (
                <div className="profile-card-bio" onClick={() => {
                  if (isSelf) { setEditingBio(true); setBioInput(bio || ""); }
                }} style={isSelf ? { cursor: "text" } : undefined}>
                  {bio || (isSelf ? "Click to add a bio..." : "")}
                </div>
              )}
            </div>
          )}

          {/* Member Since */}
          <div className="profile-card-section">
            <div className="profile-card-label">MEMBER SINCE</div>
            <div className="profile-card-value">{joinDate}</div>
          </div>

          {/* Roles */}
          {memberRoles.length > 0 && (
            <div className="profile-card-section">
              <div className="profile-card-label">ROLES</div>
              <div className="profile-card-roles">
                {memberRoles.map(r => {
                  const colorHex = typeof r.color === "number" && r.color > 0
                    ? `#${r.color.toString(16).padStart(6, "0")}`
                    : undefined;
                  return (
                    <span key={r.id} className="profile-card-role" style={{
                      borderLeftColor: colorHex || "var(--xp-border)",
                    }}>
                      {colorHex && <span className="role-dot" style={{ background: colorHex }} />}
                      {r.name}
                    </span>
                  );
                })}
              </div>
            </div>
          )}
          {!isSelf && (
            <div className="profile-card-section">
              <div className="profile-card-label">MANAGE ROLES</div>
              <div className="profile-card-roles">
                {roles.filter(r => r.name !== "@everyone").map(r => {
                  const hasRole = member.role_ids.includes(r.id);
                  return (
                    <span
                      key={r.id}
                      className={`profile-card-role ${hasRole ? "active" : ""}`}
                      style={{ cursor: "pointer" }}
                      onClick={async () => {
                        try {
                          if (hasRole) await api.removeRole(serverId, pkStr, r.id);
                          else await api.assignRole(serverId, pkStr, r.id);
                        } catch {}
                      }}
                    >
                      {hasRole ? "- " : "+ "}{r.name}
                    </span>
                  );
                })}
              </div>
            </div>
          )}
          {!isSelf && (
            <div className="profile-card-actions">
              <button className="xp-button profile-action-btn" onClick={async () => {
                try {
                  const result = await api.openDm(serverId, pkStr);
                  const dms = await api.listDms(serverId);
                  dispatch({ type: "SET_DMS", serverId, payload: dms });
                  dispatch({ type: "SELECT_CHANNEL", serverId, payload: result.channel.id });
                  const msgs = await api.fetchHistory(serverId, result.channel.id);
                  dispatch({ type: "SET_MESSAGES", serverId, payload: { channelId: result.channel.id, messages: msgs.reverse() } });
                  onClose();
                } catch (e) {
                  console.error("open dm failed:", e);
                }
              }}>Message</button>
              <button className="xp-button profile-action-btn profile-block-btn" onClick={async () => {
                try { await api.blockUser(serverId, pkStr); } catch {}
                onClose();
              }}>Block</button>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
