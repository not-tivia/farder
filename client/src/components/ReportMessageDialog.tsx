import { useState } from "react";
import * as api from "../lib/tauri-bridge";

/**
 * Report a message to the server's moderators.
 *
 * The consent checkbox is the whole point of this dialog. In an ENCRYPTED
 * channel the server cannot read the message, so a moderator has nothing to look
 * at unless the reporter hands over their own decrypted copy — and doing that
 * means putting that text into the server's database in the clear, where
 * whoever runs it can read it. That is a choice belonging to the person who can
 * still read the message, not a default the app takes on their behalf.
 *
 * So: unticked by default, and worded so the cost is legible before the click
 * rather than discovered after it. A report with no copy is still a real report
 * — a moderator can delete a message content-blind, which is how moderation in
 * an encrypted channel works at all.
 */
export default function ReportMessageDialog({
  serverId,
  channelId,
  messageId,
  eventHash,
  /** The text as THIS client can read it: decrypted for a sealed row, plain
   *  otherwise. Absent when the reader cannot read it either. */
  readableText,
  encrypted,
  onClose,
}: {
  serverId: string;
  channelId: number;
  messageId: number;
  eventHash: string | null;
  readableText: string | null;
  encrypted: boolean;
  onClose: () => void;
}) {
  const [reason, setReason] = useState("");
  const [attach, setAttach] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState(false);

  async function submit() {
    const trimmed = reason.trim();
    if (!trimmed) {
      setError("Say what is wrong with it — a moderator has only your words to go on.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.reportMessage(serverId, channelId, messageId, trimmed, {
        eventHash,
        evidence: attach ? readableText : null,
      });
      setSent(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={busy ? undefined : onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()} style={{ minWidth: 420, maxWidth: 520 }}>
        <div className="modal-titlebar">
          <span>Report this message</span>
          <button className="modal-close" onClick={onClose} disabled={busy}>X</button>
        </div>

        <div className="modal-body">
          {sent ? (
            <>
              <p className="settings-help">
                Sent. The server's moderators can see it now. You will not be told
                what they decide — a report is not a conversation.
              </p>
              <button className="xp-button" onClick={onClose}>Close</button>
            </>
          ) : (
            <>
              <div className="connect-section">
                <label className="connect-label">What is wrong with it?</label>
                <textarea
                  className="connect-input"
                  rows={3}
                  value={reason}
                  maxLength={500}
                  autoFocus
                  placeholder="Spam, harassment, illegal content…"
                  onChange={(e) => setReason(e.target.value)}
                />
              </div>

              {readableText !== null && (
                <div className="connect-section">
                  <label className="settings-row">
                    <input
                      type="checkbox"
                      checked={attach}
                      onChange={(e) => setAttach(e.target.checked)}
                    />
                    Attach a copy of the message
                  </label>
                  <p className="settings-help">
                    {encrypted ? (
                      <>
                        This channel is encrypted, so the server <strong>cannot read
                        the message</strong> — moderators have only your description
                        unless you attach your copy. Attaching it puts that text in
                        the server's database, readable by whoever runs it.
                      </>
                    ) : (
                      <>
                        This channel is not encrypted, so the server already holds
                        the message. Attaching a copy records what it said at the
                        moment you reported it, in case it is edited or deleted.
                      </>
                    )}
                  </p>
                  {attach && (
                    <div className="invite-code-display">
                      <div className="invite-code-label">What will be sent</div>
                      <div className="invite-code-value">
                        {readableText.slice(0, 400)}{readableText.length > 400 ? "…" : ""}
                      </div>
                    </div>
                  )}
                </div>
              )}

              {readableText === null && (
                <p className="settings-help">
                  You cannot read this message either, so there is no copy to
                  attach. Moderators can still delete it without reading it.
                </p>
              )}

              {error && <div className="error-text">{error}</div>}

              <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 12 }}>
                <button className="xp-button" onClick={onClose} disabled={busy}>Cancel</button>
                <button className="xp-button" onClick={() => { void submit(); }} disabled={busy}>
                  {busy ? "Sending…" : "Send report"}
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
