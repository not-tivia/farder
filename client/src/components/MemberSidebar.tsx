import { useState } from "react";
import { useServer } from "../context/ServerContext";
import type { MemberInfo } from "../lib/types";
import UserProfilePopup from "./UserProfilePopup";

export default function MemberSidebar() {
  const { state } = useServer();
  const [profilePopup, setProfilePopup] = useState<{ member: MemberInfo; x: number; y: number } | null>(null);

  // Sort members by their highest role position (descending), then by display name
  function highestRolePosition(member: MemberInfo): number {
    if (member.role_ids.length === 0) return -1;
    return Math.max(
      ...member.role_ids.map((id) => {
        const role = state.roles.find((r) => r.id === id);
        return role ? role.position : -1;
      }),
    );
  }

  const sortedMembers = [...state.members].sort((a, b) => {
    const diff = highestRolePosition(b) - highestRolePosition(a);
    if (diff !== 0) return diff;
    return a.display_name.localeCompare(b.display_name);
  });

  return (
    <div className="member-sidebar">
      <div className="member-sidebar-header">
        Members — {state.members.length}
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
      {profilePopup && (
        <UserProfilePopup
          member={profilePopup.member}
          roles={state.roles}
          position={{ x: profilePopup.x, y: profilePopup.y }}
          onClose={() => setProfilePopup(null)}
        />
      )}
    </div>
  );
}
