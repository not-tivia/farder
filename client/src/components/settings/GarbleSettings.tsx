import { useEffect, useMemo, useState } from "react";
import { MODES, MODE_LABELS, MAX_PHRASE, garbleText, makeTransform, type GarbleMode } from "../../lib/garble";
import { GARBLE_CHANGED, getGarble, setGarble, type GarbleSettings as Settings } from "../../lib/garbleSettings";

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
