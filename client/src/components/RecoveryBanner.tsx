import { useEffect, useState } from "react";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import { useRecoveryStatus } from "../hooks/useRecoveryStatus";
import { getOwnPk } from "../hooks/useServerEvents";
import { publicKeyToString, memberDisplayName } from "../lib/types";
import { describeWait, takeoverState } from "../lib/recovery";

const dismissKey = (serverId: string) => `farder.recovery-nudge.${serverId}`;

/**
 * Two small notices at the top of the window:
 *  - EVERY member sees a takeover that is waiting ("Ownership is moving to X in 2 days"), because
 *    a takeover is meant to be seen by everybody;
 *  - the OWNER of a server with no recovery key sees a one-line nudge to make one (dismissible).
 * Neither blocks clicks.
 */
export default function RecoveryBanner() {
  const server = useActiveServer();
  const serverId = useActiveServerId();
  const status = useRecoveryStatus(serverId, !!server?.connected && !server?.connectionLost && !!server?.logServerId);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  const [ownPk, setOwnPk] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => { void getOwnPk().then(setOwnPk); }, []);
  useEffect(() => { const t = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000); return () => window.clearInterval(t); }, []);
  useEffect(() => {
    if (!serverId) return;
    try { setDismissed(localStorage.getItem(dismissKey(serverId)) === "1"); } catch { setDismissed(false); }
  }, [serverId]);

  if (!status || !serverId) return null;

  if (status.pending) {
    const who = (server?.members ?? []).find((m) => publicKeyToString(m.public_key) === status.pending!.new_owner);
    const name = who ? memberDisplayName(who.display_name) : "another account";
    const st = takeoverState(status.pending, now);
    return (
      <div className="recovery-banner" role="status">
        Ownership of this server is moving to <b>{name}</b>
        {st.kind === "waiting" ? ` in ${describeWait(st.left)}.` : " now."}
        {" "}Only the recovery key can stop it.
      </div>
    );
  }

  const isOwner = !!ownPk && ownPk === (server?.ownerPublicKey ?? null);
  if (isOwner && !status.key_set && !dismissed) {
    return (
      <div className="recovery-banner nudge" role="status" style={{ pointerEvents: "auto" }}>
        This server has no recovery key. Open Server Settings, then Recovery, to make one.
        <button className="xp-button" type="button" style={{ marginLeft: 8 }} onClick={() => {
          try { localStorage.setItem(dismissKey(serverId), "1"); } catch { /* not remembered */ }
          setDismissed(true);
        }}>Not now</button>
      </div>
    );
  }
  return null;
}
