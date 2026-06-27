import { useState, useEffect, useRef, useLayoutEffect } from "react";
import { createPortal } from "react-dom";
import type { MemberInfo, RoleInfo } from "../lib/types";
import { publicKeyToString, memberDisplayName } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { useApp, useActiveServer } from "../context/ServerContext";
import { useMemberProfile } from "../hooks/useMemberProfile";
import { toast } from "../lib/toast";
import { formatPresence } from "../lib/presence";

interface Props {
  member: MemberInfo;
  roles: RoleInfo[];
  position: { x: number; y: number };
  onClose: () => void;
  isSelf?: boolean;
  serverId: string;
}

export default function UserProfilePopup({ member: initialMember, roles: initialRoles, position, onClose, isSelf, serverId }: Props) {
  const { dispatch } = useApp();
  const activeServer = useActiveServer();
  const pkStr = publicKeyToString(initialMember.public_key);

  // Read live member and roles from context so they update in real-time
  const member = activeServer?.members.find(m => publicKeyToString(m.public_key) === pkStr) ?? initialMember;
  const roles = activeServer?.roles ?? initialRoles;
  const memberRoles = roles.filter(r => member.role_ids.includes(r.id) && r.name !== "@everyone");
  const joinDate = new Date(member.joined_at * 1000).toLocaleDateString([], {
    year: "numeric", month: "short", day: "numeric"
  });
  const initial = memberDisplayName(member.display_name).charAt(0).toUpperCase();

  const defaultBannerColor = `hsl(${Math.abs(pkStr.split("").reduce((a, c) => a + c.charCodeAt(0), 0)) % 360}, 50%, 40%)`;

  const { avatarUrl: remoteAvatarUrl, status: remoteStatus } = useMemberProfile(serverId, pkStr, member.profile_hash);

  const [bio, setBio] = useState<string | null>(null);
  const [bannerColor, setBannerColor] = useState(defaultBannerColor);
  const [editingBio, setEditingBio] = useState(false);
  const [bioInput, setBioInput] = useState("");
  const [avatarUrl, setAvatarUrl] = useState<string | null>(null);
  const [overrideUrl, setOverrideUrl] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [editingStatus, setEditingStatus] = useState(false);
  const [statusInput, setStatusInput] = useState("");
  const [editingName, setEditingName] = useState(false);
  const [nameInput, setNameInput] = useState("");

  useEffect(() => {
    if (isSelf) {
      api.getBio().then(b => { if (b) setBio(b); });
      api.getProfileColor().then(c => { if (c) setBannerColor(c); });
      api.getAvatar().then(url => { if (url) setAvatarUrl(url); });
      api.getServerAvatarOverride(serverId).then(url => { if (url) setOverrideUrl(url); });
      api.getProfileStatus().then(s => { if (s) setStatus(s); });
    }
  }, [isSelf, serverId]);

  const shownAvatar = isSelf ? (overrideUrl ?? avatarUrl) : remoteAvatarUrl;

  async function saveBio() {
    const trimmed = bioInput.trim();
    await api.setBio(trimmed);
    setBio(trimmed || null);
    setEditingBio(false);
  }

  // Position the card from the click + viewport, opening TOWARD THE CHAT.
  //
  // The subtle part: with display scaling / UI zoom (e.g. Windows at 125%), the
  // click coordinates and clientWidth/Height arrive in SCREEN pixels, but the CSS
  // left/top we set are interpreted in CSS pixels and then multiplied back up by
  // the scale — so on a 125% display, left:2081 renders at 2601 and the member-
  // list popup flies off the right edge (invisible). We measure that scale
  // (rendered width ÷ layout width), compute the position in SCREEN space (where
  // the click lives), then convert to CSS px (÷ scale) so it lands correctly.
  const cardRef = useRef<HTMLDivElement>(null);
  const [scale, setScale] = useState(1);
  useLayoutEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    const measured = el.getBoundingClientRect().width / (el.offsetWidth || 300);
    if (measured > 0.1 && Math.abs(measured - scale) > 0.01) setScale(measured);
  });
  const M = 8;
  const CARD_W = 300; // .profile-card CSS width
  const vw = document.documentElement.clientWidth || window.innerWidth;   // screen px
  const vh = document.documentElement.clientHeight || window.innerHeight; // screen px
  const cardW = CARD_W * scale; // the card's on-screen width
  // Right-half clicks (member list) open leftward; bottom-half clicks anchor the
  // card's bottom so it grows upward. Computed in screen space, clamped on-screen.
  let leftScreen = position.x > vw / 2 ? position.x - cardW : position.x;
  leftScreen = Math.max(M, Math.min(leftScreen, vw - cardW - M));
  const openUp = position.y > vh / 2;
  const topScreen = Math.max(M, position.y);
  const bottomScreen = Math.max(M, vh - position.y);
  const style: React.CSSProperties = {
    position: "fixed",
    left: leftScreen / scale, // screen px → CSS px so the post-zoom result is right
    right: "auto",
    ...(openUp
      ? { bottom: bottomScreen / scale, top: "auto" }
      : { top: topScreen / scale, bottom: "auto" }),
    maxHeight: `${(vh - 2 * M) / scale}px`,
    overflowY: "auto",
    zIndex: 1000,
  };

  return createPortal(
    <>
      <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={onClose} />
      <div ref={cardRef} className="profile-card" style={style}>
        {/* Banner */}
        <div className="profile-card-banner" style={{ background: bannerColor }} />

        {/* Avatar */}
        <div className="profile-card-avatar-row">
          <div className="profile-card-avatar" style={{ background: shownAvatar ? "none" : bannerColor }}>
            {shownAvatar ? (
              <img src={shownAvatar} alt="avatar" style={{ width: "100%", height: "100%", borderRadius: "50%", objectFit: "cover" }} />
            ) : (
              initial
            )}
          </div>
          {isSelf && (
            <div className="avatar-change-group">
              <button className="avatar-change-btn" onClick={async () => {
                const path = await api.pickFile();
                if (path) {
                  try {
                    const url = await api.setAvatar(path);
                    setAvatarUrl(url);
                  } catch (err) {
                    toast.error(`Couldn't set avatar: ${err}`);
                  }
                }
              }}>Change</button>
              <button className="avatar-change-btn" onClick={async () => {
                const path = await api.pickFile();
                if (path) {
                  try {
                    const url = await api.setServerAvatarOverride(serverId, path);
                    setOverrideUrl(url);
                  } catch (err) {
                    toast.error(`Couldn't set server avatar: ${err}`);
                  }
                }
              }}>This server</button>
              {overrideUrl && (
                <button className="avatar-change-btn" onClick={async () => {
                  await api.clearServerAvatarOverride(serverId);
                  setOverrideUrl(null);
                }}>Reset</button>
              )}
            </div>
          )}
        </div>

        {/* Info */}
        <div className="profile-card-body">
          {isSelf && editingName ? (
            <input
              className="profile-card-status-input"
              value={nameInput}
              maxLength={128}
              autoFocus
              onChange={(e) => setNameInput(e.target.value)}
              onKeyDown={async (e) => {
                if (e.key === "Enter") {
                  const v = nameInput.trim();
                  if (!v) { toast.error("Display name cannot be empty"); return; }
                  try {
                    await api.setDisplayName(v);
                    setEditingName(false); // member list refreshes via member_profile_updated
                  } catch (err) { toast.error(`Couldn't save name: ${err}`); }
                }
                if (e.key === "Escape") setEditingName(false);
              }}
              onBlur={() => setEditingName(false)}
              placeholder="Set a display name..."
            />
          ) : (
            <div
              className="profile-card-name"
              onClick={isSelf ? () => {
                const cur = member.display_name;
                const isPlaceholder = !cur || /^vk_[0-9a-f]{8}$/.test(cur);
                setNameInput(isPlaceholder ? "" : cur);
                setEditingName(true);
              } : undefined}
              style={isSelf ? { cursor: "text", display: "inline-flex", alignItems: "center", gap: 6 } : undefined}
              title={isSelf ? "Click to edit your display name" : undefined}
            >
              <span>{memberDisplayName(member.display_name)}</span>
              {isSelf && (
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                  strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
                  style={{ opacity: 0.55, flexShrink: 0 }} aria-hidden="true">
                  <path d="M12 20h9" />
                  <path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5Z" />
                </svg>
              )}
            </div>
          )}
          <div className="profile-card-id">{pkStr.slice(0, 18)}...</div>

          {member.presence && <div className="profile-presence">{formatPresence(member.presence)}</div>}

          {(isSelf || remoteStatus) && (
            <div className="profile-card-status">
              {isSelf ? (
                editingStatus ? (
                  <input
                    className="profile-card-status-input"
                    value={statusInput}
                    maxLength={128}
                    autoFocus
                    onChange={(e) => setStatusInput(e.target.value)}
                    onKeyDown={async (e) => {
                      if (e.key === "Enter") {
                        const v = statusInput.trim() || null;
                        try {
                          await api.setProfileStatus(v);
                          setStatus(v);
                          setEditingStatus(false);
                        } catch (err) { toast.error(`Couldn't save status: ${err}`); }
                      }
                      if (e.key === "Escape") setEditingStatus(false);
                    }}
                    placeholder="Set a status..."
                  />
                ) : (
                  <span onClick={() => { setEditingStatus(true); setStatusInput(status || ""); }} style={{ cursor: "text" }}>
                    {status || "Set a status..."}
                  </span>
                )
              ) : (
                <span>{remoteStatus}</span>
              )}
            </div>
          )}

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
                  return (
                    <span key={r.id} className="profile-card-role" style={{
                      borderLeftColor: r.color || "var(--xp-border)",
                    }}>
                      {r.color && <span className="role-dot" style={{ background: r.color }} />}
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
                          // Refresh members to get updated role_ids
                          const members = await api.getMembers(serverId);
                          dispatch({ type: "SET_MEMBERS", serverId, payload: members });
                        } catch (e) {
                          console.error("role toggle failed:", e);
                        }
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
    </>,
    document.body,
  );
}
