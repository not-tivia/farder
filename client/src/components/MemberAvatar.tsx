import { useMemberProfile } from "../hooks/useMemberProfile";

interface Props {
  serverId: string;
  publicKey?: string;            // omit when unknown -> always letter fallback
  profileHash?: string | null;
  name: string;
  className: string;             // keeps each site's existing class (member-avatar-mini, message-avatar, ...)
}

export default function MemberAvatar({ serverId, publicKey, profileHash, name, className }: Props) {
  const { avatarUrl } = useMemberProfile(serverId, publicKey ?? "", publicKey ? profileHash : null);
  return (
    <span className={className}>
      {avatarUrl
        ? <img className="avatar-img" src={avatarUrl} alt="" />
        : (name || "?").charAt(0).toUpperCase()}
    </span>
  );
}
