import { useState } from "react";
import * as api from "../lib/tauri-bridge";

// Duration is REQUIRED for giveaways (server syntax: /<trigger> <duration> <prize>).
const DURATIONS: { value: string; label: string }[] = [
  { value: "30m", label: "30 minutes" },
  { value: "1h", label: "1 hour" },
  { value: "6h", label: "6 hours" },
  { value: "1d", label: "1 day" },
  { value: "3d", label: "3 days" },
  { value: "7d", label: "7 days" },
];

interface Props {
  serverId: string;
  channelId: number;
  /** The giveaway command's trigger word (without "/"). */
  trigger: string;
  onClose: () => void;
  /** Called after the giveaway was successfully created (modal closes itself). */
  onCreated: () => void;
}

export default function GiveawayBuilderModal({ serverId, channelId, trigger, onClose, onCreated }: Props) {
  const [prize, setPrize] = useState("");
  const [duration, setDuration] = useState("1h");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleCreate() {
    if (submitting) return;
    setError(null);
    const p = prize.trim();
    if (!p) { setError("Prize is required"); return; }
    if (p.length > 200) { setError("Prize must be at most 200 characters"); return; }
    // Server syntax: <duration> <prize>
    const args = `${duration} ${p}`;
    setSubmitting(true);
    try {
      await api.runCommand(serverId, trigger, channelId, args);
      onCreated();
    } catch (e) {
      // e.g. the server's MANAGE_SERVER refusal for non-mods — stay open.
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Start Giveaway</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          <div className="connect-section">
            <label className="connect-label">Prize</label>
            <input
              className="connect-input"
              type="text"
              value={prize}
              maxLength={200}
              placeholder="What are you giving away?"
              onChange={(e) => setPrize(e.target.value)}
              autoFocus
            />
          </div>
          <div className="connect-section">
            <label className="connect-label">Duration</label>
            <select
              className="connect-input"
              value={duration}
              onChange={(e) => setDuration(e.target.value)}
            >
              {DURATIONS.map((d) => (
                <option key={d.value} value={d.value}>{d.label}</option>
              ))}
            </select>
          </div>
          {error && <div className="error-text">{error}</div>}
          <div className="connect-actions">
            <button className="xp-button" onClick={handleCreate} disabled={submitting}>
              {submitting ? "Starting…" : "Create"}
            </button>
            <button className="xp-button" onClick={onClose} disabled={submitting}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
