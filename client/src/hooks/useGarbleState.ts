import { useCallback, useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import type { GarbleState } from "../lib/tauri-bridge";

const EMPTY: GarbleState = { rules: [], consents: [], can_moderate: false };

/**
 * What garbling touches me on this server: rules on me or set by me, consents I
 * gave or hold, and whether I may start a moderation garble. Refetched whenever
 * the server says something changed (`farder:garble-changed`, dispatched by
 * useServerEvents). Fails closed: if the server cannot be asked (a v1 server, a
 * dropped connection) there is simply no garble state.
 */
/** `loaded`: the first answer (or failure) is in, so a menu built from `state` will not change shape under the pointer. */
export function useGarbleState(serverId: string | null): { state: GarbleState; ownPk: string | null; refresh: () => void; loaded: boolean } {
  const [state, setState] = useState<GarbleState>(EMPTY);
  const [loaded, setLoaded] = useState(false);
  const [ownPk, setOwnPk] = useState<string | null>(null);

  const refresh = useCallback(() => {
    if (!serverId) { setState(EMPTY); setLoaded(true); return; }
    api.getGarbleState(serverId).then(setState).catch(() => setState(EMPTY)).finally(() => setLoaded(true));
  }, [serverId]);

  useEffect(() => {
    refresh();
    const onChange = (e: Event) => {
      const id = (e as CustomEvent<{ serverId: string }>).detail?.serverId;
      if (!id || id === serverId) refresh();
    };
    window.addEventListener("farder:garble-changed", onChange);
    // `can_moderate` depends on the powers I hold, so a grant/revoke refreshes it too.
    window.addEventListener("farder:capabilities-changed", onChange);
    return () => {
      window.removeEventListener("farder:garble-changed", onChange);
      window.removeEventListener("farder:capabilities-changed", onChange);
    };
  }, [serverId, refresh]);

  useEffect(() => {
    let alive = true;
    api.getPublicKey().then((pk) => { if (alive) setOwnPk(pk ?? null); }).catch(() => {});
    return () => { alive = false; };
  }, []);

  return { state, ownPk, refresh, loaded };
}
