import { useState } from "react";
import * as api from "../lib/tauri-bridge";

const DURATIONS: { value: string; label: string }[] = [
  { value: "15m", label: "15 minutes" },
  { value: "30m", label: "30 minutes" },
  { value: "1h", label: "1 hour" },
  { value: "3h", label: "3 hours" },
  { value: "1d", label: "1 day" },
  { value: "3d", label: "3 days" },
  { value: "7d", label: "7 days" },
  { value: "custom", label: "Custom…" },
];

// Server bounds (reminders.rs MIN_DURATION_SECS / MAX_DURATION_SECS /
// MAX_REMINDER_TEXT) — mirrored client-side for a friendly error; the server
// re-validates from scratch.
const MIN_DURATION_SECS = 60; // 1m
const MAX_DURATION_SECS = 30 * 86_400; // 30d
const MAX_REMINDER_TEXT = 500;
const UNIT_SECS: Record<string, number> = { m: 60, h: 3600, d: 86_400 };

/**
 * Resolves the duration <select> value plus custom amount/unit into the exact
 * `\d{1,4}(m|h|d)` token the server's duration regex accepts. Returns null when
 * the custom combo violates the 1m–30d bounds.
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
  /** The reminder command's trigger word (without "/"). */
  trigger: string;
  onClose: () => void;
  /** Called after the reminder was set; carries the server's Notice text. */
  onCreated: (notice: string | null) => void;
}

export default function ReminderBuilderModal({ serverId, channelId, trigger, onClose, onCreated }: Props) {
  const [duration, setDuration] = useState("1h");
  const [customAmount, setCustomAmount] = useState("");
  const [customUnit, setCustomUnit] = useState("m");
  const [text, setText] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleCreate() {
    if (submitting) return;
    setError(null);
    // Client-side validation mirroring the server's parse_reminder_args rules.
    const token = resolveDurationToken(duration, customAmount, customUnit);
    if (token === null) {
      setError("Duration must be between 1 minute and 30 days");
      return;
    }
    const body = text.trim();
    if (!body) { setError("Reminder text is required"); return; }
    if (body.length > MAX_REMINDER_TEXT) {
      setError(`Reminder text must be at most ${MAX_REMINDER_TEXT} characters`);
      return;
    }
    // The reminder grammar splits on the FIRST whitespace only, so the text is
    // taken verbatim — no delimiter to strip (unlike the poll/event syntax).
    setSubmitting(true);
    try {
      const notice = await api.runCommand(serverId, trigger, channelId, `${token} ${body}`);
      onCreated(notice);
    } catch (e) {
      // Server-side rejection (rate limit, pending cap, permissions) — stay open.
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Set Reminder</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          <div className="connect-section">
            <label className="connect-label">Remind me in</label>
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
                  placeholder="45"
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
          <div className="connect-section">
            <label className="connect-label">Reminder text {text.length}/{MAX_REMINDER_TEXT}</label>
            <textarea
              className="connect-input"
              value={text}
              maxLength={MAX_REMINDER_TEXT}
              rows={3}
              placeholder="take the pizza out"
              onChange={(e) => setText(e.target.value)}
              autoFocus
            />
          </div>
          <div style={{ color: "var(--xp-text-muted)", fontSize: 12 }}>
            Private — nothing is posted. The server DMs you when it's due.
          </div>
          {error && <div className="error-text">{error}</div>}
          <div className="connect-actions">
            <button className="xp-button" onClick={handleCreate} disabled={submitting}>
              {submitting ? "Setting…" : "Set reminder"}
            </button>
            <button className="xp-button" onClick={onClose} disabled={submitting}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
