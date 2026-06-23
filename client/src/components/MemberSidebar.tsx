import { useState, useEffect } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import type { MemberInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import UserProfilePopup from "./UserProfilePopup";
import MemberContextMenu from "./MemberContextMenu";
import TimedOutBadge from "./TimedOutBadge";
import { getActorPermissions, isModerator } from "../lib/permissions";
import MemberAvatar from "./MemberAvatar";
import { useMemberProfile } from "../hooks/useMemberProfile";
import { formatPresence } from "../lib/presence";

// Module-level cache for own public key
let cachedOwnPk: string | null = null;

function MemberRow({ member, serverId, showModBadges, onClick, onContextMenu }: {
  member: MemberInfo;
  serverId: string;
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
        name={member.display_name}
      />
      <span className="online-dot" />
      <span className="member-text">
        <span className="member-name">{member.display_name}</span>
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

  function highestRolePosition(member: MemberInfo): number {
    if (member.role_ids.length === 0) return -1;
    return Math.max(
      ...member.role_ids.map((id) => {
        const role = roles.find((r) => r.id === id);
        return role ? role.position : -1;
      }),
    );
  }

  const sortedMembers = [...members].sort((a, b) => {
    const diff = highestRolePosition(b) - highestRolePosition(a);
    if (diff !== 0) return diff;
    return a.display_name.localeCompare(b.display_name);
  });

  return (
    <div className="member-sidebar">
      <div className="member-sidebar-header">
        Members — {members.length}
      </div>
      <div className="member-list">
        {serverId && sortedMembers.map((member) => (
          <MemberRow
            key={member.public_key.bytes.join(",")}
            member={member}
            serverId={serverId}
            showModBadges={showModBadges}
            onClick={(e) => setProfilePopup({ member, x: e.clientX, y: e.clientY })}
            onContextMenu={(e) => {
              e.preventDefault();
              setContextMenu({ target: member, position: { x: e.clientX, y: e.clientY } });
            }}
          />
        ))}
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
