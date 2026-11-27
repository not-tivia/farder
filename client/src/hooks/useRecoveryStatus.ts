import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

/**
 * The recovery status of one server (whether a recovery key is set, and a takeover that is
 * waiting), kept fresh: asked again whenever the server announces a membership change (every
 * recovery step is announced that way) and once a minute as a backstop. `null` until known, and
 * for a server that has no signed history (nothing to recover).
 */
export function useRecoveryStatus(serverId: string | null, connected: boolean): api.RecoveryStatus | null {
  const [status, setStatus] = useState<api.RecoveryStatus | null>(null);
  useEffect(() => {
    if (!serverId || !connected) { setStatus(null); return; }
    let cancelled = false;
    const ask = () => { api.recoveryStatus(serverId).then((s) => { if (!cancelled) setStatus(s); }).catch(() => { if (!cancelled) setStatus(null); }); };
    ask();
    const onChange = (e: Event) => {
      const id = (e as CustomEvent<{ serverId: string }>).detail?.serverId;
      if (!id || id === serverId) ask();
    };
    window.addEventListener("farder:capabilities-changed", onChange);
    const t = window.setInterval(ask, 60_000);
    return () => { cancelled = true; window.removeEventListener("farder:capabilities-changed", onChange); window.clearInterval(t); };
  }, [serverId, connected]);
  return status;
}
