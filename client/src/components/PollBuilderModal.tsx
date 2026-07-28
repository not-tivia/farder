import { useState } from "react";
import * as api from "../lib/tauri-bridge";

// The server's /poll arg syntax uses "|" as the segment delimiter
// (question | option | option [| duration]). A literal pipe typed into the
// question or an option would be parsed as a segment break server-side, so we
// replace it with "/" at input time — the closest visually-safe stand-in.
function stripPipes(s: string): string {
  return s.replace(/\|/g, "/");
}

const DURATIONS: { value: string; label: string }[] = [
  { value: "", label: "No end time" },
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
  /** The poll command's trigger word (without "/"). */
  trigger: string;
  onClose: () => void;
  /** Called after the poll was successfully created (modal closes itself). */
  onCreated: () => void;
}

export default function PollBuilderModal({ serverId, channelId, trigger, onClose, onCreated }: Props) {
  const [question, setQuestion] = useState("");
  const [options, setOptions] = useState<string[]>(["", ""]);
  const [duration, setDuration] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function setOption(i: number, value: string) {
    setOptions((prev) => prev.map((o, idx) => (idx === i ? stripPipes(value) : o)));
  }

  function addOption() {
    setOptions((prev) => (prev.length < 10 ? [...prev, ""] : prev));
  }

  function removeOption(i: number) {
    setOptions((prev) => (prev.length > 2 ? prev.filter((_, idx) => idx !== i) : prev));
  }

  async function handleCreate() {
    if (submitting) return;
    setError(null);
    // Client-side validation mirroring the server's parse_poll_args rules.
    const q = question.trim();
    if (!q) { setError("Question is required"); return; }
    if (q.length > 256) { setError("Question must be at most 256 characters"); return; }
    const opts = options.map((o) => o.trim());
    if (opts.some((o) => !o)) { setError("Options cannot be empty"); return; }
    if (opts.some((o) => o.length > 100)) { setError("Options must be at most 100 characters"); return; }
    const seen = new Set<string>();
    for (const o of opts) {
      const key = o.toLowerCase();
      if (seen.has(key)) { setError(`Duplicate option: "${o}"`); return; }
      seen.add(key);
    }
    // Build the exact server syntax: <question> | <opt> | <opt>[ | <token>]
    const args = [q, ...opts, ...(duration ? [duration] : [])].join(" | ");
    setSubmitting(true);
    try {
      await api.runCommand(serverId, trigger, channelId, args);
      onCreated();
    } catch (e) {
      // Server-side rejection (rate limit, permissions, parse) — stay open.
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Create Poll</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          <div className="connect-section">
            <label className="connect-label">Question</label>
            <input
              className="connect-input"
              type="text"
              value={question}
              maxLength={256}
              placeholder="What should we vote on?"
              onChange={(e) => setQuestion(stripPipes(e.target.value))}
              autoFocus
            />
          </div>
          <div className="connect-section">
            <label className="connect-label">Options ({options.length}/10)</label>
            {options.map((opt, i) => (
              <div key={i} style={{ display: "flex", gap: 4, marginBottom: 4 }}>
                <input
                  className="connect-input"
                  type="text"
                  value={opt}
                  maxLength={100}
                  placeholder={`Option ${i + 1}`}
                  onChange={(e) => setOption(i, e.target.value)}
                  style={{ flex: 1 }}
                />
                {options.length > 2 && (
                  <button
                    className="xp-button"
                    onClick={() => removeOption(i)}
                    title="Remove option"
                  >
                    X
                  </button>
                )}
              </div>
            ))}
            {options.length < 10 && (
              <button className="xp-button" onClick={addOption}>+ Add option</button>
            )}
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
              {submitting ? "Creating…" : "Create"}
            </button>
            <button className="xp-button" onClick={onClose} disabled={submitting}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
