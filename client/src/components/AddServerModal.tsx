import { useState } from "react";
import { useApp } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

function parseInviteLink(input: string): {
  address?: string;
  inviteCode?: string;
  setupToken?: string;
} {
  const trimmed = input.trim();
  if (!trimmed) return {};

  // farder.gg/join/ENCODED format
  const joinMatch = trimmed.match(/(?:https?:\/\/)?farder\.gg\/join\/([A-Za-z0-9_-]+)/);
  if (joinMatch) {
    try {
      const decoded = atob(joinMatch[1].replace(/-/g, "+").replace(/_/g, "/"));
      const slashIdx = decoded.indexOf("/");
      if (slashIdx > 0) {
        const address = decoded.substring(0, slashIdx);
        const token = decoded.substring(slashIdx + 1);
        if (token.startsWith("setup:")) {
          return { address, setupToken: token.slice(6) };
        }
        return { address, inviteCode: token };
      }
    } catch {}
  }

  // farder:// protocol link
  const farderMatch = trimmed.match(/^farder:\/\/([^/]+)\/(.+)$/i);
  if (farderMatch) {
    const address = farderMatch[1];
    const token = farderMatch[2];
    if (token.startsWith("setup:")) {
      return { address, setupToken: token.slice(6) };
    }
    return { address, inviteCode: token };
  }

  // host:port/code format (without farder://)
  const slashMatch = trimmed.match(/^([^/]+:\d+)\/(.+)$/);
  if (slashMatch) {
    const address = slashMatch[1];
    const token = slashMatch[2];
    if (token.startsWith("setup:")) {
      return { address, setupToken: token.slice(6) };
    }
    return { address, inviteCode: token };
  }

  // 64-char hex = standalone setup token
  if (/^[0-9a-f]{64}$/i.test(trimmed)) {
    return { setupToken: trimmed };
  }

  // host:port only (no invite)
  if (/^.+:\d+$/.test(trimmed)) {
    return { address: trimmed };
  }

  // Short string = invite code
  return { inviteCode: trimmed };
}

export default function AddServerModal({ onClose }: { onClose: () => void }) {
  const { dispatch } = useApp();
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleJoin() {
    setLoading(true);
    setError(null);
    try {
      const parsed = parseInviteLink(input.trim());
      const address = parsed.address;
      if (!address) {
        setError("Enter a server address or invite link");
        setLoading(false);
        return;
      }
      const result = await api.connectServer(address, parsed.inviteCode, parsed.setupToken);
      dispatch({ type: "SERVER_ADDED", serverId: address, payload: result });
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: address });
      try {
        const members = await api.getMembers(address);
        dispatch({ type: "SET_MEMBERS", serverId: address, payload: members });
      } catch {}
      try {
        const dms = await api.listDms(address);
        dispatch({ type: "SET_DMS", serverId: address, payload: dms });
      } catch {}
      onClose();
    } catch (e) {
      setError(String(e));
    }
    setLoading(false);
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Add Server</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          <label className="connect-label">Paste an invite link or server address</label>
          <input
            className="connect-input"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") handleJoin(); }}
            placeholder="farder.gg/join/..."
            autoFocus
          />
          {error && <div className="error-text">{error}</div>}
          <div className="connect-actions" style={{ marginTop: 8 }}>
            <button className="xp-button" onClick={handleJoin} disabled={loading}>
              {loading ? "Joining..." : "Join Server"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
