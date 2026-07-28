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
  { value: "custom", label: "Custom…" },
];

// Server bounds (giveaways.rs MIN_DURATION_SECS / MAX_DURATION_SECS) — mirrored
// client-side for a friendly error; the server re-validates from scratch.
const MIN_DURATION_SECS = 60; // 1m
const MAX_DURATION_SECS = 30 * 86_400; // 30d
const UNIT_SECS: Record<string, number> = { m: 60, h: 3600, d: 86_400 };

/**
 * Resolves the duration <select> value plus custom amount/unit into the exact
 * `\d{1,4}(m|h|d)` token the server's duration regex accepts. Returns null
 * when the custom combo violates the 1m–30d bounds.
 */
function resolveDurationToken(duration: string, amount: string, unit: string): string | null {
  if (duration !== "custom") return duration;
  const n = Number(amount);
  if (!Number.isInteger(n) || n < 1 || n > 9999) return null;
  const secs = n * UNIT_SECS[unit];
  if (secs < MIN_DURATION_SECS || secs > MAX_DURATION_SECS) return null;
  return `${n}${unit}`;
}

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
  const [customAmount, setCustomAmount] = useState("");
  const [customUnit, setCustomUnit] = useState("m");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleCreate() {
    if (submitting) return;
    setError(null);
    const p = prize.trim();
    if (!p) { setError("Prize is required"); return; }
    if (p.length > 200) { setError("Prize must be at most 200 characters"); return; }
    // Resolve the duration token (custom builds `${n}${unit}`; always required).
    const token = resolveDurationToken(duration, customAmount, customUnit);
    if (token === null) {
      setError("Duration must be between 1 minute and 30 days");
      return;
    }
    // Server syntax: <duration> <prize>
    const args = `${token} ${p}`;
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
            {duration === "custom" && (
              <div style={{ display: "flex", gap: 4, marginTop: 4 }}>
                <input
                  className="connect-input"
                  type="number"
                  min={1}
                  max={9999}
                  step={1}
                  value={customAmount}
                  placeholder="90"
                  onChange={(e) => setCustomAmount(e.target.value)}
                  style={{ width: 80 }}
                />
                <select
                  className="connect-input"
                  value={customUnit}
                  onChange={(e) => setCustomUnit(e.target.value)}
                >
                  <option value="m">minutes</option>
                  <option value="h">hours</option>
                  <option value="d">days</option>
                </select>
              </div>
            )}
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
