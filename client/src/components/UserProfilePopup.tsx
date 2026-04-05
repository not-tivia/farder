import type { MemberInfo, RoleInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";

interface Props {
    member: MemberInfo;
    roles: RoleInfo[];
    position: { x: number; y: number };
    onClose: () => void;
}

export default function UserProfilePopup({ member, roles, position, onClose }: Props) {
    const pkStr = publicKeyToString(member.public_key);
    const memberRoles = roles.filter(r => member.role_ids.includes(r.id));
    const joinDate = new Date(member.joined_at * 1000).toLocaleDateString([], {
        year: "numeric", month: "short", day: "numeric"
    });

    // Clamp position so popup stays on screen
    const popupWidth = 240;
    const popupHeight = 200; // approximate
    const x = Math.min(position.x, window.innerWidth - popupWidth - 8);
    const y = Math.min(position.y, window.innerHeight - popupHeight - 8);

    return (
        <>
            <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={onClose} />
            <div className="user-profile-popup" style={{ top: y, left: x }}>
                <div className="profile-header">
                    <div className="profile-avatar">{member.display_name.charAt(0).toUpperCase()}</div>
                    <div className="profile-name">{member.display_name}</div>
                </div>
                <div className="profile-body">
                    <div className="profile-section">
                        <div className="profile-label">Member Since</div>
                        <div className="profile-value">{joinDate}</div>
                    </div>
                    {memberRoles.length > 0 && (
                        <div className="profile-section">
                            <div className="profile-label">Roles</div>
                            <div className="profile-roles">
                                {memberRoles.map(r => (
                                    <span
                                        key={r.id}
                                        className="profile-role-badge"
                                        style={{
                                            borderColor: typeof r.color === "number"
                                                ? `#${r.color.toString(16).padStart(6, "0")}`
                                                : undefined
                                        }}
                                    >
                                        {r.name}
                                    </span>
                                ))}
                            </div>
                        </div>
                    )}
                    <div className="profile-section">
                        <div className="profile-label">ID</div>
                        <div className="profile-value profile-id">{pkStr.slice(0, 20)}...</div>
                    </div>
                </div>
            </div>
        </>
    );
}
