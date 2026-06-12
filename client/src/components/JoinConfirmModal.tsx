import { useState } from "react";
import { useInvitePreview } from "../hooks/useInvitePreview";

export default function JoinConfirmModal({
  relayed,
  link,
  onConfirm,
  onCancel,
}: {
  relayed: boolean;
  link?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [showInfo, setShowInfo] = useState(false);
  const preview = useInvitePreview(link);
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Join server</span>
          <button className="modal-close" onClick={onCancel}>X</button>
        </div>
        <div className="modal-body">
          <p>
            {preview.status === "ok" && preview.serverName
              ? <>You&apos;ve been invited to <strong>{preview.serverName}</strong>. Join it?</>
              : <>You&apos;ve been invited to a Farder server. Join it?</>}
          </p>
          <div className={`join-relay-note ${relayed ? "relayed" : "direct"}`}>
            <span className="join-relay-badge">{relayed ? "RELAYED" : "DIRECT"}</span>
            <span>
              {relayed
                ? "This server uses a relay — your IP address stays hidden from the host."
                : "Direct server — the host can see your IP address."}
            </span>
          </div>
          <button type="button" className="learn-more-toggle" onClick={() => setShowInfo(!showInfo)}>
            {showInfo ? "Hide details" : "Learn more"}
          </button>
          {showInfo && (
            <div className="learn-more-body">
              <p>A relay is a neutral middle server. Connecting through it means the server&apos;s host never learns your IP address (and you never learn theirs).</p>
              <p>Your direct messages and voice are end-to-end encrypted either way. Community channel messages are readable by the server host &mdash; and, today, by the relay operator on relayed servers; hardening that is on the roadmap.</p>
            </div>
          )}
          <div className="connect-actions">
            <button className="xp-button" onClick={onConfirm}>Join</button>
            <button className="xp-button" onClick={onCancel}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
