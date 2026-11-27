import { useCallback, useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString, memberDisplayName } from "../lib/types";
import { describeWait, looksLikeRecoveryPhrase, normalizePhrase, takeoverState } from "../lib/recovery";

interface Props {
  serverId: string;
  logServerId: string;
  /** This account is the owner right now. */
  isOwner: boolean;
  /** This account's own key (the default person to hand the server to). */
  ownPk: string | null;
}

const box = (tone: "warn" | "info"): React.CSSProperties => ({
  border: `1px solid ${tone === "warn" ? "var(--xp-e2ee-error)" : "var(--xp-border)"}`,
  background: tone === "warn" ? "var(--xp-e2ee-error-soft)" : "var(--xp-panel-bg)",
  color: "var(--xp-text-normal)",
  borderRadius: 3, padding: 8, marginBottom: 10, fontSize: "var(--xp-font-size)",
});

/**
 * The recovery key: a second, offline key that can take the server back if the everyday owner
 * key is lost or stolen, after a public waiting period. Every member can see it (a takeover is
 * meant to be seen); only the owner can make the key; only someone holding the 24 words can
 * start, cancel or finish a takeover.
 */
export default function RecoveryTab({ serverId, logServerId, isOwner, ownPk }: Props) {
  const server = useActiveServer();
  const [status, setStatus] = useState<api.RecoveryStatus | null>(null);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [shownPhrase, setShownPhrase] = useState<string | null>(null);
  const [wroteItDown, setWroteItDown] = useState(false);
  const [words, setWords] = useState("");
  const [newOwner, setNewOwner] = useState<string>(ownPk ?? "");

  const refresh = useCallback(() => {
    api.recoveryStatus(serverId).then(setStatus).catch((e) => setError(String(e)));
  }, [serverId]);

  useEffect(() => {
    refresh();
    // A recovery step is announced to everyone as a membership change.
    const onChange = (e: Event) => {
      const id = (e as CustomEvent<{ serverId: string }>).detail?.serverId;
      if (!id || id === serverId) refresh();
    };
    window.addEventListener("farder:capabilities-changed", onChange);
    const tick = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => { window.removeEventListener("farder:capabilities-changed", onChange); window.clearInterval(tick); };
  }, [serverId, refresh]);

  const people = (server?.members ?? []).filter((m) => !m.is_bot);
  const nameOf = (pk: string) => {
    const m = people.find((x) => publicKeyToString(x.public_key) === pk);
    return m ? memberDisplayName(m.display_name) : pk.slice(0, 12) + "...";
  };

  async function run(what: string, fn: () => Promise<void>) {
    setBusy(true); setError(null); setNote(null);
    try { await fn(); setNote(what); refresh(); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }

  const state = takeoverState(status?.pending ?? null, now);
  const phraseOk = looksLikeRecoveryPhrase(words);

  return (
    <div style={{ padding: 8 }}>
      <div style={box("info")}>
        A <b>recovery key</b> is a second key, written on paper and kept away from this computer. If
        your everyday account is ever lost or taken over by someone else, the recovery key can take the
        server back. It always waits for <b>{status ? describeWait(status.wait_secs) : "a few days"}</b> first,
        and every member can see it happening. Your everyday account cannot stop it.
      </div>

      {!status && !error && <div>Loading...</div>}

      {status && !status.key_set && (
        isOwner ? (
          <>
            <div style={box("warn")}>
              This server has <b>no recovery key</b>. If your account were stolen, nothing could stop the
              thief, and if it were lost nobody could take the server over. Make one now.
            </div>
            {!shownPhrase && (
              <button className="xp-button" disabled={busy}
                onClick={() => run("Recovery key made. Write the words down before you close this.", async () => {
                  const r = await api.recoveryCreate(serverId, logServerId);
                  setShownPhrase(r.phrase); setWroteItDown(false);
                })}>
                Make a recovery key
              </button>
            )}
          </>
        ) : (
          <div style={box("info")}>The owner has not made a recovery key for this server yet.</div>
        )
      )}

      {shownPhrase && (
        <div style={box("warn")}>
          <div style={{ fontWeight: "bold", marginBottom: 6 }}>Write these 24 words down, in order. This is the only time they are shown.</div>
          <textarea className="connect-input" readOnly rows={4} value={shownPhrase}
            style={{ width: "100%", boxSizing: "border-box", fontFamily: "monospace" }}
            onFocus={(e) => e.currentTarget.select()} aria-label="Recovery words" />
          <div style={{ marginTop: 6, fontSize: 11 }}>
            Keep them on paper, away from this computer. Anyone who has them can take this server after the
            waiting time, and your everyday account cannot stop them. If you lose them, make a new key.
          </div>
          <label style={{ display: "block", marginTop: 6 }}>
            <input type="checkbox" checked={wroteItDown} onChange={(e) => setWroteItDown(e.target.checked)} />
            {" "}I wrote the 24 words down
          </label>
          <button className="xp-button" disabled={!wroteItDown} style={{ marginTop: 6 }}
            onClick={() => { setShownPhrase(null); setNote("The words are gone from this screen."); }}>
            Hide the words
          </button>
        </div>
      )}

      {status?.key_set && !shownPhrase && (
        <div style={box("info")}>A recovery key is set for this server.</div>
      )}

      {status?.pending && (
        <div style={box("warn")}>
          <b>Ownership of this server is moving to {nameOf(status.pending.new_owner)}.</b>
          <div>
            {state.kind === "waiting"
              ? `It will be able to finish in ${describeWait(state.left)}.`
              : "The waiting time is over: it can finish now."}
          </div>
          <div style={{ fontSize: 11, marginTop: 4 }}>
            Only the holder of the recovery key can stop it. If this is a surprise, tell the owner
            another way.
          </div>
        </div>
      )}

      {status?.key_set && (
        <div className="organizer-create" style={{ marginTop: 12 }}>
          <div style={{ fontWeight: "bold", marginBottom: 4 }}>I have the recovery key</div>
          <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginBottom: 6 }}>
            Use this on a NEW account that has joined the server like any member. Type the 24 words, choose
            who becomes the owner, and start. The words are used once and never saved.
          </div>
          <textarea className="connect-input" rows={3} value={words} placeholder="word1 word2 word3 ... (24 words)"
            style={{ width: "100%", boxSizing: "border-box" }} onChange={(e) => setWords(e.target.value)} aria-label="The 24 recovery words" />
          <div style={{ margin: "6px 0" }}>
            <label>Make this person the owner:{" "}
              <select className="connect-input" value={newOwner} onChange={(e) => setNewOwner(e.target.value)}>
                {people.map((m) => {
                  const pk = publicKeyToString(m.public_key);
                  return <option key={pk} value={pk}>{memberDisplayName(m.display_name)}{pk === ownPk ? " (me)" : ""}</option>;
                })}
              </select>
            </label>
          </div>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            <button className="xp-button" disabled={busy || !phraseOk || !newOwner || !!status.pending}
              onClick={() => run("Takeover started. Everyone can see it now.", () => api.recoveryClaim(serverId, logServerId, normalizePhrase(words), newOwner))}>
              Start the takeover
            </button>
            <button className="xp-button" disabled={busy || !phraseOk || state.kind !== "ready"}
              onClick={() => run("Done. The new owner is in charge.", () => api.recoveryFinalize(serverId, logServerId, normalizePhrase(words)))}>
              Finish the takeover
            </button>
            <button className="xp-button" disabled={busy || !phraseOk || !status.pending}
              onClick={() => run("Takeover cancelled.", () => api.recoveryCancel(serverId, logServerId, normalizePhrase(words)))}>
              Cancel the takeover
            </button>
            <button className="xp-button" disabled={busy || !phraseOk || !!status.pending}
              onClick={() => run("New recovery key made. Write the new words down; the old ones no longer work.", async () => {
                const r = await api.recoveryRotate(serverId, logServerId, normalizePhrase(words));
                setWords(""); setShownPhrase(r.phrase); setWroteItDown(false);
              })}>
              Replace the recovery key
            </button>
          </div>
          {!phraseOk && words.trim() !== "" && <div style={{ fontSize: 11, marginTop: 4 }}>That is not 24 words yet.</div>}
        </div>
      )}

      {note && <div style={{ marginTop: 8 }}>{note}</div>}
      {error && <div className="error-text" style={{ marginTop: 8 }}>{error}</div>}
    </div>
  );
}
