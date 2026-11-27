import { useEffect, useState } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

/**
 * Shown while this server is reached through a backup address instead of the host: the host is offline and
 * you are reading a saved copy. Sending waits until the host is back; the app keeps asking the host and
 * switches back by itself.
 */
export default function SavedCopyBanner() {
  const server = useActiveServer();
  const serverId = useActiveServerId();
  const [route, setRoute] = useState<string | null>(null);
  const live = !!server?.connected && !server?.connectionLost;

  useEffect(() => {
    if (!serverId || !live) { setRoute(null); return; }
    let cancelled = false;
    const look = () => api.getConnectionRoute(serverId).then((r) => { if (!cancelled) setRoute(r); }).catch(() => {});
    look();
    const t = window.setInterval(look, 4000);
    return () => { cancelled = true; window.clearInterval(t); };
  }, [serverId, live]);

  if (!route) return null;
  return (
    <div className="recovery-banner" role="status">
      The host is offline. You are reading a saved copy kept at <b>{route}</b>: you can read the history, but
      sending waits until the host is back (the app keeps checking and switches back by itself).
    </div>
  );
}
