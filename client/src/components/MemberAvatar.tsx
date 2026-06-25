import { useEffect, useRef, useState } from "react";
import { useMemberProfile } from "../hooks/useMemberProfile";
import { useDataSaver } from "../context/DataSaverContext";
import { isAnimatedDataUrl } from "../lib/dataSaver";

interface Props {
  serverId: string;
  publicKey?: string;            // omit when unknown -> always letter fallback
  profileHash?: string | null;
  name: string;
  className: string;             // keeps each site's existing class (member-avatar-mini, message-avatar, ...)
}

export default function MemberAvatar({ serverId, publicKey, profileHash, name, className }: Props) {
  const { avatarUrl } = useMemberProfile(serverId, publicKey ?? "", publicKey ? profileHash : null);
  const { settings } = useDataSaver();
  const freeze = settings.enabled && settings.freezeAvatars && isAnimatedDataUrl(avatarUrl);

  return (
    <span className={className}>
      {!avatarUrl
        ? (name || "?").charAt(0).toUpperCase()
        : freeze
          ? <FrozenAvatar src={avatarUrl} />
          : <img className="avatar-img" src={avatarUrl} alt="" />}
    </span>
  );
}

// Draws the first frame of an animated image into a canvas so it stops moving.
// Pure render-time: the bytes are already downloaded/cached by profile-sync.
function FrozenAvatar({ src }: { src: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
    const img = new Image();
    img.onload = () => {
      const c = canvasRef.current;
      if (!c) return;
      const w = img.naturalWidth || 64;
      const h = img.naturalHeight || 64;
      c.width = w;
      c.height = h;
      const ctx = c.getContext("2d");
      if (!ctx) { setFailed(true); return; }
      try { ctx.drawImage(img, 0, 0, w, h); } catch { setFailed(true); }
    };
    img.onerror = () => setFailed(true);
    img.src = src;
    return () => { img.onload = null; img.onerror = null; };
  }, [src]);

  // Fall back to the (animated) image rather than a blank avatar on any failure.
  if (failed) return <img className="avatar-img" src={src} alt="" />;
  return <canvas ref={canvasRef} className="avatar-img" />;
}
