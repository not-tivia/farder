import { useEffect, useMemo, useState } from "react";
import { MODES, MODE_LABELS, MAX_PHRASE, garbleText, makeTransform, type GarbleMode } from "../../lib/garble";
import { GARBLE_CHANGED, getGarble, setGarble, type GarbleSettings as Settings } from "../../lib/garbleSettings";
import * as api from "../../lib/tauri-bridge";
import { useApp, useActiveServer } from "../../context/ServerContext";
import { useGarbleState } from "../../hooks/useGarbleState";
import { publicKeyToString } from "../../lib/types";

const SAMPLE = "Hello there, how are you doing today?";

/**
 * "Speak as..." -- turn your own messages into barks, meows, mumbles or jumbles.
 *
 * It is YOUR choice about YOUR words, applied on your device just before a
 * message is sent. Others receive the garbled words; there is no hidden mode.
 * While it is on, the message box shows a badge with a one-click Turn off.
 */
export default function GarbleSettings() {
    const [s, setS] = useState<Settings>(getGarble);

    useEffect(() => {
        const onChange = () => setS(getGarble());
        window.addEventListener(GARBLE_CHANGED, onChange);
        return () => window.removeEventListener(GARBLE_CHANGED, onChange);
    }, []);

    function save(next: Settings) {
        setS(next);
        setGarble(next);
    }

    // A fresh sample whenever the mode or phrase changes (random modes vary).
    const preview = useMemo(
        () => garbleText(SAMPLE, makeTransform(s.mode, s.phrase)),
        [s.mode, s.phrase],
    );

    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Speech Garbler</h2>
            <GarbledByOthers />
            <div className="connect-section">
                <div className="connect-section-title">Speak as...</div>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                    <input type="checkbox" checked={s.enabled}
                        onChange={e => save({ ...s, enabled: e.target.checked })} />
                    <span>Garble everything I send</span>
                </label>
                <select className="connect-input" value={s.mode}
                    onChange={e => save({ ...s, mode: e.target.value as GarbleMode })}>
                    {MODES.map(m => <option key={m} value={m}>{MODE_LABELS[m]}</option>)}
                </select>
                {s.mode === "custom" && (
                    <input className="connect-input" style={{ marginTop: 6 }} maxLength={MAX_PHRASE}
                        placeholder="What every word becomes" value={s.phrase}
                        onChange={e => save({ ...s, phrase: e.target.value })} />
                )}
                <div style={{ marginTop: 10, fontSize: 11, color: "var(--xp-text-muted)" }}>Preview</div>
                <div style={{ marginTop: 2, color: "var(--xp-text-normal)" }}>{preview}</div>
                <div style={{ marginTop: 10, fontSize: 11, color: "var(--xp-text-muted)" }}>
                    This changes the words of your own messages, on this device, just before they are sent.
                    Everyone, including you, sees the garbled version. Links, @mentions, :emoji: and code
                    are left alone, and slash commands are never changed. A badge above the message box
                    shows while this is on, with a button to turn it off.
                </div>
            </div>
        </div>
    );
}

/**
 * The other direction: letting someone else garble ME. Nothing happens unless I
 * name a person and a limit here, I can see everything running on me, and the
 * kill switch for a consensual garble is the Stop button below. (A moderator's
 * garble is not stoppable by me, and says so.)
 */
function GarbledByOthers() {
    const { state: app } = useApp();
    const server = useActiveServer();
    const serverId = app.activeServerId;
    const logServerId = server?.logServerId ?? null;
    const { state, ownPk, refresh } = useGarbleState(logServerId ? serverId : null);
    const [pick, setPick] = useState("");
    const [max, setMax] = useState(25);
    const [error, setError] = useState<string | null>(null);

    if (!server || !serverId || !logServerId) return null;

    const nameOf = (pk: string) =>
        server.members.find((m) => publicKeyToString(m.public_key) === pk)?.display_name ?? pk.slice(0, 10);
    const candidates = server.members.filter(
        (m) => !m.is_bot && publicKeyToString(m.public_key) !== ownPk,
    );
    const onMe = state.rules.filter((r) => r.target === ownPk);
    const given = state.consents.filter((c) => c.target === ownPk);

    async function run(fn: () => Promise<void>) {
        setError(null);
        try { await fn(); refresh(); } catch (e) { setError(String(e)); }
    }

    return (
        <div className="connect-section">
            <div className="connect-section-title">Being garbled by others (on {server.serverName})</div>
            {onMe.length === 0 && (
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>Nobody is garbling you right now.</div>
            )}
            {onMe.map((r) => (
                <div key={r.by + r.mode} style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                    <span style={{ flex: 1 }}>
                        {MODE_LABELS[r.mode as GarbleMode] ?? r.mode} by {r.moderation ? "a moderator" : nameOf(r.by)}
                        {" "}-- {r.remaining} message{r.remaining === 1 ? "" : "s"} left
                    </span>
                    {r.moderation
                        ? <span style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>moderator action: you cannot stop this</span>
                        : <button className="xp-button" onClick={() => void run(() => api.garbleClear(serverId, logServerId, r.target))}>Stop</button>}
                </div>
            ))}

            <div style={{ marginTop: 10, fontWeight: "bold" }}>Let someone garble me</div>
            <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginBottom: 6 }}>
                They can then garble up to this many of your messages at a time. You can stop it or take
                the permission back at any moment.
            </div>
            <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                <select className="connect-input" value={pick} onChange={(e) => setPick(e.target.value)}>
                    <option value="">Choose a person...</option>
                    {candidates.map((m) => {
                        const pk = publicKeyToString(m.public_key);
                        return <option key={pk} value={pk}>{m.display_name}</option>;
                    })}
                </select>
                <input className="connect-input" type="number" min={1} max={500} value={max} style={{ width: 80 }}
                    onChange={(e) => setMax(Number(e.target.value))} />
                <button className="xp-button" disabled={!pick}
                    onClick={() => void run(async () => {
                        await api.garbleConsent(serverId, logServerId, pick, Math.max(1, Math.min(500, Math.floor(max) || 1)));
                        setPick("");
                    })}>Allow</button>
            </div>
            {given.map((c) => (
                <div key={c.controller} style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 4 }}>
                    <span style={{ flex: 1 }}>{nameOf(c.controller)} may garble up to {c.max_messages} at a time</span>
                    <button className="xp-button" onClick={() => void run(() => api.garbleConsent(serverId, logServerId, c.controller, 0))}>Take back</button>
                </div>
            ))}
            {error && <div className="error-text" style={{ marginTop: 6 }}>{error}</div>}
        </div>
    );
}
