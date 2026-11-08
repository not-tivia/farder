import { useMemo, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import { MODES, MODE_LABELS, MAX_PHRASE, displayGarble, type GarbleMode } from "../lib/garble";

interface Props {
  serverId: string;
  logServerId: string;
  targetPk: string;
  targetName: string;
  /** I may start a moderation garble (hold "garble"). */
  canModerate: boolean;
  /** Most messages they let ME garble (0 = they have not consented to me). */
  consentMax: number;
  /** A rule is running on them that I can end. */
  running: boolean;
  onClose: () => void;
}

const MODERATION_MAX = 50; // keep in step with GARBLE_MODERATION_MAX_MESSAGES

const overlay: CSSProperties = {
  position: "fixed", inset: 0, background: "rgba(0,0,0,0.4)",
  display: "flex", alignItems: "center", justifyContent: "center", zIndex: 2400,
};
const card: CSSProperties = {
  background: "var(--xp-window-bg)", color: "var(--xp-text-normal)",
  border: "2px solid var(--xp-blue-dark)", padding: 20, width: 440,
  fontFamily: "var(--xp-font)",
};

/**
 * Garble another member's next few messages. Two kinds, never confusable:
 *  - MODERATION: needs the "garble" capability; labelled "garbled by a moderator"
 *    on every message, recorded in the audit log, at most 50 messages.
 *  - WITH CONSENT: they earlier allowed me to; bounded by what they allowed, and
 *    they can stop it any time.
 * The words are never changed in the log; viewers draw them garbled.
 */
export default function GarbleMemberDialog({
  serverId, logServerId, targetPk, targetName, canModerate, consentMax, running, onClose,
}: Props) {
  const [kind, setKind] = useState<"moderation" | "consent">(canModerate ? "moderation" : "consent");
  const [mode, setMode] = useState<GarbleMode>("bark");
  const [phrase, setPhrase] = useState("");
  const [messages, setMessages] = useState(10);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const limit = kind === "moderation" ? MODERATION_MAX : consentMax;
  const count = Math.max(1, Math.min(limit, Math.floor(messages) || 1));
  const preview = useMemo(
    () => displayGarble("Hello there, how are you?", { mode, phrase }, "preview"),
    [mode, phrase],
  );
  const phraseOk = mode !== "custom" || phrase.trim().length > 0;

  async function start() {
    setBusy(true); setError(null);
    try {
      await api.garbleSet({
        serverId, logServerId, target: targetPk, mode,
        phrase: mode === "custom" ? phrase.trim() : "", messages: count,
        consensual: kind === "consent",
      });
      onClose();
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }
  async function stop() {
    setBusy(true); setError(null);
    try { await api.garbleClear(serverId, logServerId, targetPk); onClose(); }
    catch (e) { setError(String(e)); } finally { setBusy(false); }
  }

  return (
    <div style={overlay} onClick={onClose}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ marginTop: 0 }}>Garble {targetName}</h3>

        {canModerate && consentMax > 0 && (
          <div style={{ display: "flex", flexDirection: "column", gap: 4, marginBottom: 8, fontSize: 11 }}>
            <label><input type="radio" checked={kind === "moderation"} onChange={() => setKind("moderation")} />{" "}
              As a moderator (everyone sees it is moderation; recorded in the audit log)</label>
            <label><input type="radio" checked={kind === "consent"} onChange={() => setKind("consent")} />{" "}
              With their consent (up to {consentMax} messages)</label>
          </div>
        )}
        <p style={{ fontSize: 11, color: "var(--xp-text-muted)", marginTop: 0 }}>
          {kind === "moderation"
            ? "Their next messages are shown to everyone garbled and labelled as moderation. Their real words stay in the log, and they see their own messages as written."
            : "They agreed to this. Their next messages are shown garbled, and they can stop it whenever they like."}
        </p>

        <select className="connect-input" value={mode} onChange={(e) => setMode(e.target.value as GarbleMode)}>
          {MODES.map((m) => <option key={m} value={m}>{MODE_LABELS[m]}</option>)}
        </select>
        {mode === "custom" && (
          <input className="connect-input" style={{ marginTop: 6 }} maxLength={MAX_PHRASE}
            placeholder="What every word becomes" value={phrase} onChange={(e) => setPhrase(e.target.value)} />
        )}
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 8, fontSize: 11 }}>
          <span>For their next</span>
          <input className="connect-input" type="number" min={1} max={limit} value={messages}
            style={{ width: 70 }} onChange={(e) => setMessages(Number(e.target.value))} />
          <span>messages (most {limit})</span>
        </div>
        <div style={{ marginTop: 10, fontSize: 11, color: "var(--xp-text-muted)" }}>Preview</div>
        <div>{preview}</div>

        {error && <div className="error-text" style={{ marginTop: 8 }}>{error}</div>}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 14 }}>
          {running && <button className="xp-button" disabled={busy} onClick={() => void stop()}>Stop garbling</button>}
          <button className="xp-button" onClick={onClose}>Cancel</button>
          <button className="xp-button" disabled={busy || !phraseOk || limit < 1} onClick={() => void start()}>Start</button>
        </div>
      </div>
    </div>
  );
}
