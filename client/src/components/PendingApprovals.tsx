import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../lib/tauri-bridge";
import type { MemberInfo } from "../lib/types";
import { publicKeyToString, memberDisplayName } from "../lib/types";
import { useActiveServer } from "../context/ServerContext";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";

interface Props {
  serverId: string;
}

export default function PendingApprovals({ serverId }: Props) {
  const activeServer = useActiveServer();
  const members = activeServer?.members ?? [];
  const roles = activeServer?.roles ?? [];
  const ownerPublicKey = activeServer?.ownerPublicKey ?? null;
  const logServerId = activeServer?.logServerId ?? null;

  // Mirror MemberContextMenu.tsx lines 79-86: resolve viewer permissions.
  // ownPk is loaded asynchronously.
  const [resolvedOwnPk, setResolvedOwnPk] = useState<string | null>(null);
  useEffect(() => {
    api.getPublicKey().then((pk) => setResolvedOwnPk(pk)).catch(() => {});
  }, []);

  const { bits } = resolvedOwnPk
    ? getActorPermissions(members, roles, resolvedOwnPk, ownerPublicKey)
    : { bits: 0n };

  const isOwner = resolvedOwnPk !== null && resolvedOwnPk === ownerPublicKey;
  const canApprove = isOwner || hasPermission(bits, PERMISSIONS.KICK_MEMBERS);

  const [pending, setPending] = useState<MemberInfo[]>([]);
  const [error, setError] = useState<string | null>(null);

  function fetchPending() {
    api.getPendingMembers(serverId)
      .then((list) => setPending(list))
      .catch((e) => setError(String(e)));
  }

  // Fetch on mount (and whenever serverId / canApprove flips true).
  useEffect(() => {
    if (!canApprove) return;
    fetchPending();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverId, canApprove]);

  // Re-fetch when a membership change fires for this server.
  useEffect(() => {
    if (!canApprove) return;
    const unlistenPromise = listen<{ server_id: string }>(
      "server:membership_changed",
      (e) => {
        if (e.payload.server_id === serverId) fetchPending();
      },
    );
    return () => {
      unlistenPromise.then((u) => u()).catch(() => {});
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverId, canApprove]);

  // Not an approver, or nothing pending — render nothing.
  if (!canApprove || pending.length === 0) return null;

  async function handleApprove(m: MemberInfo) {
    if (!logServerId) return;
    const pk = publicKeyToString(m.public_key);
    try {
      await api.approveMember(serverId, logServerId, pk);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleDeny(m: MemberInfo) {
    if (!logServerId) return;
    const pk = publicKeyToString(m.public_key);
    try {
      await api.denyMember(serverId, logServerId, pk);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="pending-approvals-section">
      <div className="member-role-group pending-approvals-header">
        Pending requests ({pending.length})
      </div>
      {pending.map((m) => {
        const pkStr = publicKeyToString(m.public_key);
        return (
          <div key={pkStr} className="member-item pending-approval-item">
            <span className="member-name" style={{ flex: 1, minWidth: 0 }}>
              {memberDisplayName(m.display_name)}
            </span>
            <span className="pending-approval-actions">
              <button
                className="pending-approve-btn"
                title="Approve"
                onClick={() => void handleApprove(m)}
              >
                ✓
              </button>
              <button
                className="pending-deny-btn"
                title="Deny"
                onClick={() => void handleDeny(m)}
              >
                ✗
              </button>
            </span>
          </div>
        );
      })}
      {error && (
        <div style={{ color: "var(--xp-red, #a00)", fontSize: 10, padding: "2px 10px" }}>
          {error}
        </div>
      )}
    </div>
  );
}
