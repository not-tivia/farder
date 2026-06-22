import { useState } from "react";
import { providerLabel, type EmbedProvider } from "../lib/embedPlayer";

export default function EmbedConsentModal({
  provider,
  onConfirm,
  onCancel,
}: {
  provider: EmbedProvider;
  onConfirm: (alwaysAllow: boolean) => void;
  onCancel: () => void;
}) {
  const [alwaysAllow, setAlwaysAllow] = useState(false);
  const label = providerLabel(provider);
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Watch in Farder?</span>
          <button className="modal-close" onClick={onCancel}>X</button>
        </div>
        <div className="modal-body">
          <p>
            Playing this connects you directly to <strong>{label}</strong> and
            shares your IP address and viewing data with them. Farder can&apos;t
            hide this while the video plays.
          </p>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={alwaysAllow}
              onChange={(e) => setAlwaysAllow(e.target.checked)}
            />
            Always allow {label} embeds
          </label>
          <div className="connect-actions">
            <button className="xp-button" onClick={() => onConfirm(alwaysAllow)}>Watch</button>
            <button className="xp-button" onClick={onCancel}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
