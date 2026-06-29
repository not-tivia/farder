import { useState, useEffect, useRef, Fragment } from "react";
import { getMemberSidebarWidth, setMemberSidebarWidth, clampMemberWidth, MEMBER_SIDEBAR_DEFAULT } from "../lib/memberWidth";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import type { MemberInfo, RoleInfo } from "../lib/types";
import { publicKeyToString, memberDisplayName } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import UserProfilePopup from "./UserProfilePopup";
import MemberContextMenu from "./MemberContextMenu";
import PendingApprovals from "./PendingApprovals";
import TimedOutBadge from "./TimedOutBadge";
import { getActorPermissions, isModerator } from "../lib/permissions";
import MemberAvatar from "./MemberAvatar";
import { useMemberProfile } from "../hooks/useMemberProfile";
import { formatPresence } from "../lib/presence";

// Module-level cache for own public key
let cachedOwnPk: string | null = null;

// Returns the member's highest-position hoisted role (excl @everyone), or null.
function hoistGroup(member: MemberInfo, roles: RoleInfo[]): RoleInfo | null {
  const hoisted = roles
    .filter(r => r.hoist && r.name !== "@everyone" && member.role_ids.includes(r.id))
    .sort((a, b) => b.position - a.position);
  return hoisted[0] ?? null;
}

function nameColor(member: MemberInfo, roles: RoleInfo[]): string | undefined {
  const mine = roles
    .filter(r => r.name !== "@everyone" && r.color && member.role_ids.includes(r.id))
    .sort((a, b) => b.position - a.position);
  return mine[0]?.color ?? undefined;
}

function MemberRow({ member, serverId, roles, showModBadges, onClick, onContextMenu }: {
  member: MemberInfo;
  serverId: string;
  roles: RoleInfo[];
  showModBadges: boolean;
  onClick: (e: React.MouseEvent) => void;
  onContextMenu: (e: React.MouseEvent) => void;
}) {
  const pkStr = publicKeyToString(member.public_key);
  const { status } = useMemberProfile(serverId, pkStr, member.profile_hash);
  return (
    <div className="member-item" onClick={onClick} onContextMenu={onContextMenu}>
      <MemberAvatar
        className="member-avatar-mini"
        serverId={serverId}
        publicKey={pkStr}
        profileHash={member.profile_hash}
        name={memberDisplayName(member.display_name)}
      />
      <span className="online-dot" />
      <span className="member-text">
        <span className="member-name" style={{ color: nameColor(member, roles) }}>{memberDisplayName(member.display_name)}</span>
        {member.presence
          ? <span className="member-presence" title={formatPresence(member.presence)}>{formatPresence(member.presence)}</span>
          : status && <span className="member-status" title={status}>{status}</span>}
      </span>
      {showModBadges && (
        <TimedOutBadge untilMs={member.timeout_until} reason={member.timeout_reason} />
      )}
    </div>
  );
}

export default function MemberSidebar() {
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const [profilePopup, setProfilePopup] = useState<{ member: MemberInfo; x: number; y: number } | null>(null);
  const [contextMenu, setContextMenu] = useState<{ target: MemberInfo; position: { x: number; y: number } } | null>(null);
  const [ownPk, setOwnPk] = useState(cachedOwnPk);

  // Draggable width (handle on the left edge). Persisted + clamped.
  const [width, setWidth] = useState(getMemberSidebarWidth());
  const resizeRef = useRef<{ startX: number; base: number; raf: number } | null>(null);
  const startResize = (e: React.PointerEvent) => {
    resizeRef.current = { startX: e.clientX, base: width, raf: 0 };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };
  const onResizeMove = (e: React.PointerEvent) => {
    const d = resizeRef.current; if (!d) return;
    // Handle is on the LEFT edge; dragging left (smaller clientX) widens the bar.
    const next = clampMemberWidth(d.base + (d.startX - e.clientX));
    if (!d.raf) d.raf = requestAnimationFrame(() => { d.raf = 0; setWidth(next); });
  };
  const endResize = (e: React.PointerEvent) => {
    const d = resizeRef.current; if (!d) return;
    if (d.raf) cancelAnimationFrame(d.raf);
    resizeRef.current = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    const final = clampMemberWidth(d.base + (d.startX - e.clientX));
    setWidth(final);            // keep state in sync with the persisted value (no end-of-drag flicker)
    setMemberSidebarWidth(final);
  };
  const resetWidth = () => { setWidth(MEMBER_SIDEBAR_DEFAULT); setMemberSidebarWidth(MEMBER_SIDEBAR_DEFAULT); };

  useEffect(() => {
    if (!cachedOwnPk) {
      api.getPublicKey().then(pk => { cachedOwnPk = pk; setOwnPk(pk); });
    }
  }, []);

  const members = activeServer?.members ?? [];
  const roles = activeServer?.roles ?? [];

  const { bits: viewerBits } = ownPk
    ? getActorPermissions(members, roles, ownPk, activeServer?.ownerPublicKey ?? null)
    : { bits: 0n };
  const showModBadges = isModerator(viewerBits);

  // Sort all members by display name within each section.
  const nameSortedMembers = [...members].sort((a, b) =>
    memberDisplayName(a.display_name).localeCompare(memberDisplayName(b.display_name))
  );

  // Build sections: one per hoisted role that has ≥1 member (highest position first),
  // then a catch-all "Members" section for members with no hoisted role.
  const hoistedRoleMap = new Map<number, { role: RoleInfo; members: MemberInfo[] }>();
  const ungroupedMembers: MemberInfo[] = [];

  for (const member of nameSortedMembers) {
    const role = hoistGroup(member, roles);
    if (role) {
      if (!hoistedRoleMap.has(role.id)) {
        hoistedRoleMap.set(role.id, { role, members: [] });
      }
      hoistedRoleMap.get(role.id)!.members.push(member);
    } else {
      ungroupedMembers.push(member);
    }
  }

  // Sort sections by role position descending (highest rank first).
  const sections = [...hoistedRoleMap.values()].sort((a, b) => b.role.position - a.role.position);

  return (
    <div className="member-sidebar" style={{ width, minWidth: width, position: "relative" }}>
      <div
        className="member-sidebar-resize"
        title="Drag to resize · double-click to reset"
        onPointerDown={startResize}
        onPointerMove={onResizeMove}
        onPointerUp={endResize}
        onPointerCancel={endResize}
        onDoubleClick={resetWidth}
      />
      <div className="member-sidebar-header">
        Members — {members.length}
      </div>
      {serverId && <PendingApprovals serverId={serverId} />}
      <div className="member-list">
        {serverId && (
          <>
            {sections.map(({ role, members: sectionMembers }) => (
              <Fragment key={role.id}>
                <div className="member-role-group">{role.name}</div>
                {sectionMembers.map((member) => (
                  <MemberRow
                    key={member.public_key.bytes.join(",")}
                    member={member}
                    serverId={serverId}
                    roles={roles}
                    showModBadges={showModBadges}
                    onClick={(e) => setProfilePopup({ member, x: e.clientX, y: e.clientY })}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setContextMenu({ target: member, position: { x: e.clientX, y: e.clientY } });
                    }}
                  />
                ))}
              </Fragment>
            ))}
            {ungroupedMembers.length > 0 && (
              <>
                <div className="member-role-group">Members</div>
                {ungroupedMembers.map((member) => (
                  <MemberRow
                    key={member.public_key.bytes.join(",")}
                    member={member}
                    serverId={serverId}
                    roles={roles}
                    showModBadges={showModBadges}
                    onClick={(e) => setProfilePopup({ member, x: e.clientX, y: e.clientY })}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setContextMenu({ target: member, position: { x: e.clientX, y: e.clientY } });
                    }}
                  />
                ))}
              </>
            )}
          </>
        )}
      </div>
      {profilePopup && serverId && (
        <UserProfilePopup
          member={profilePopup.member}
          roles={roles}
          position={{ x: profilePopup.x, y: profilePopup.y }}
          onClose={() => setProfilePopup(null)}
          isSelf={ownPk === publicKeyToString(profilePopup.member.public_key)}
          serverId={serverId}
        />
      )}
      {contextMenu && serverId && (
        <MemberContextMenu
          target={contextMenu.target}
          serverId={serverId}
          position={contextMenu.position}
          ownPk={ownPk}
          onClose={() => setContextMenu(null)}
        />
      )}
    </div>
  );
}
