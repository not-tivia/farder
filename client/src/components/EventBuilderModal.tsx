import { useState } from "react";
import * as api from "../lib/tauri-bridge";
import type { EventInfo } from "../lib/types";

// The server's event arg syntax uses "|" as the segment delimiter
// (title | when [| location] [| description] [| remind 1h]). A literal pipe
// typed into a field would be parsed as a segment break server-side, so we
// replace it with "/" at input time — the PollBuilderModal idiom.
function stripPipes(s: string): string {
  return s.replace(/\|/g, "/");
}

/** Reminder-lead choices: the `remind <token>` the parser accepts, plus the
 *  seconds `editEvent` takes. Server bound: remind_lead ∈ {900, 3600, 86400}. */
const LEADS: { value: string; label: string; secs: number | null }[] = [
  { value: "", label: "None", secs: null },
  { value: "15m", label: "15 minutes before", secs: 900 },
  { value: "1h", label: "1 hour before", secs: 3600 },
  { value: "1d", label: "1 day before", secs: 86_400 },
];

// Server bounds (channel_events.rs MIN_LEAD_SECS / MAX_AHEAD_SECS) — mirrored
// client-side for a friendly error; the server re-validates from scratch.
const MIN_AHEAD_SECS = 60;
const MAX_AHEAD_SECS = 365 * 86_400;
const MAX_TITLE = 120;
const MAX_LOCATION = 120;
const MAX_DESCRIPTION = 500;

/** Local-time "YYYY-MM-DD" / "HH:MM" for the date/time inputs — deliberately
 *  NOT toISOString(), which would shift a prefilled event by the UTC offset. */
function pad(n: number): string {
  return String(n).padStart(2, "0");
}
function localDateValue(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
function localTimeValue(d: Date): string {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
function leadValueFromSecs(secs: number | null): string {
  return LEADS.find((l) => l.secs === secs)?.value ?? "";
}

interface Props {
  serverId: string;
  channelId: number;
  /** Create mode: the event command's trigger (without "/"). */
  trigger?: string;
  /** Edit mode: the event being edited (mutually exclusive with `trigger`). */
  editing?: EventInfo;
  onClose: () => void;
  /** Called after a successful create/edit (the parent closes the modal). */
  onCreated: () => void;
}

export default function EventBuilderModal({ serverId, channelId, trigger, editing, onClose, onCreated }: Props) {
  const startDate = editing ? new Date(editing.starts_at * 1000) : null;
  const [title, setTitle] = useState(editing?.title ?? "");
  const [date, setDate] = useState(startDate ? localDateValue(startDate) : "");
  const [time, setTime] = useState(startDate ? localTimeValue(startDate) : "");
  const [location, setLocation] = useState(editing?.location ?? "");
  const [description, setDescription] = useState(editing?.description ?? "");
  const [lead, setLead] = useState(leadValueFromSecs(editing?.remind_lead ?? null));
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit() {
    if (submitting) return;
    setError(null);
    // Client-side validation mirroring the server's parse/validate rules.
    const t = title.trim();
    if (!t) { setError("Title is required"); return; }
    if (t.length > MAX_TITLE) { setError(`Title must be at most ${MAX_TITLE} characters`); return; }
    if (!date || !time) { setError("Pick a date and a time"); return; }
    const loc = location.trim();
    if (loc.length > MAX_LOCATION) { setError(`Location must be at most ${MAX_LOCATION} characters`); return; }
    const desc = description.trim();
    if (desc.length > MAX_DESCRIPTION) { setError(`Description must be at most ${MAX_DESCRIPTION} characters`); return; }

    // A date-time string WITHOUT a Z/offset is parsed by JS as LOCAL time —
    // precisely the intent ("8pm my time"). The absolute second is what
    // travels and what is stored; nothing timezone-shaped is transmitted, and
    // every viewer renders it with toLocaleString().
    const startsAt = Math.floor(new Date(`${date}T${time}`).getTime() / 1000);
    if (!Number.isFinite(startsAt)) { setError("That date and time isn't valid"); return; }
    const now = Math.floor(Date.now() / 1000);
    if (startsAt <= now + MIN_AHEAD_SECS) { setError("Pick a time in the future"); return; }
    if (startsAt > now + MAX_AHEAD_SECS) { setError("Events can be at most a year out"); return; }

    setSubmitting(true);
    try {
      if (editing) {
        const leadSecs = LEADS.find((l) => l.value === lead)?.secs ?? null;
        await api.editEvent(serverId, editing.id, t, desc || null, loc || null, startsAt, leadSecs);
      } else {
        // Build the exact server syntax: <title> | @<unix>[ | <location>][ |
        // <description>][ | remind <lead>]. The grammar is positional, so the
        // location segment is emitted — possibly empty — whenever there is a
        // description.
        const args = [
          t,
          `@${startsAt}`,
          ...(loc || desc ? [loc] : []),
          ...(desc ? [desc] : []),
          ...(lead ? [`remind ${lead}`] : []),
        ].join(" | ");
        await api.runCommand(serverId, trigger ?? "", channelId, args);
      }
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
          <span>{editing ? "Edit Event" : "Create Event"}</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          <div className="connect-section">
            <label className="connect-label">Title</label>
            <input
              className="connect-input"
              type="text"
              value={title}
              maxLength={MAX_TITLE}
              placeholder="Movie night"
              onChange={(e) => setTitle(stripPipes(e.target.value))}
              autoFocus
            />
          </div>
          <div className="connect-section">
            <label className="connect-label">Date</label>
            <input
              className="connect-input"
              type="date"
              value={date}
              onChange={(e) => setDate(e.target.value)}
            />
          </div>
          <div className="connect-section">
            <label className="connect-label">Time (your local time)</label>
            <input
              className="connect-input"
              type="time"
              value={time}
              onChange={(e) => setTime(e.target.value)}
            />
          </div>
          <div className="connect-section">
            <label className="connect-label">Location (optional)</label>
            <input
              className="connect-input"
              type="text"
              value={location}
              maxLength={MAX_LOCATION}
              placeholder="Voice channel, my place, …"
              onChange={(e) => setLocation(stripPipes(e.target.value))}
            />
          </div>
          <div className="connect-section">
            <label className="connect-label">Description (optional) {description.length}/{MAX_DESCRIPTION}</label>
            <textarea
              className="connect-input"
              value={description}
              maxLength={MAX_DESCRIPTION}
              rows={3}
              placeholder="Anything else people should know"
              onChange={(e) => setDescription(stripPipes(e.target.value))}
            />
          </div>
          <div className="connect-section">
            <label className="connect-label">Reminder</label>
            <select
              className="connect-input"
              value={lead}
              onChange={(e) => setLead(e.target.value)}
            >
              {LEADS.map((l) => (
                <option key={l.value} value={l.value}>{l.label}</option>
              ))}
            </select>
          </div>
          {error && <div className="error-text">{error}</div>}
          <div className="connect-actions">
            <button className="xp-button" onClick={handleSubmit} disabled={submitting}>
              {submitting ? (editing ? "Saving…" : "Creating…") : (editing ? "Save" : "Create")}
            </button>
            <button className="xp-button" onClick={onClose} disabled={submitting}>Cancel</button>
          </div>
        </div>
      </div>
    </div>
  );
}
