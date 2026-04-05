import { useState, useEffect } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import type { MemberInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import UserProfilePopup from "./UserProfilePopup";

// Module-level cache for own public key
let cachedOwnPk: string | null = null;

export default function MemberSidebar() {
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const [profilePopup, setProfilePopup] = useState<{ member: MemberInfo; x: number; y: number } | null>(null);
  const [ownPk, setOwnPk] = useState(cachedOwnPk);

  useEffect(() => {
    if (!cachedOwnPk) {
      api.getPublicKey().then(pk => { cachedOwnPk = pk; setOwnPk(pk); });
    }
  }, []);

  const members = activeServer?.members ?? [];
  const roles = activeServer?.roles ?? [];

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
        {sortedMembers.map((member) => (
          <div
            key={member.public_key.bytes.join(",")}
            className="member-item"
            onClick={(e) => setProfilePopup({ member, x: e.clientX, y: e.clientY })}
          >
            <span className="online-dot" />
            <span className="member-name">{member.display_name}</span>
          </div>
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
    </div>
  );
}
