import { useEffect, useRef, useState } from "react";
import * as api from "../../lib/tauri-bridge";
import { AUDIO_LIMITS_CHANGED } from "../../lib/audioLimiter";

// Keep in step with audio_limit.rs (CEILING_MIN_DB / CEILING_MAX_DB / MASTER_MAX).
const CEIL_MIN = -40;
const CEIL_MAX = -3;
const MASTER_MAX = 1.5;

/** What a ceiling means in plain terms: the loudest a sound may be, as a share of full volume. */
function describeCeiling(db: number): string {
    const pct = Math.round(Math.pow(10, db / 20) * 100);
    return `${db} dB (nothing louder than ${pct}% of full volume)`;
}

/**
 * One control for how loud Farder may ever be, for everything it plays: voice
 * calls, shared-screen audio, and videos and voice messages in chat.
 *
 * Two sliders on purpose. Volume scales sound up or down. Max loudness is a
 * ceiling: whatever would be louder than it is turned down until it is not, so
 * a shout, a mic blast or a loud clip cannot hurt, however high the volume is.
 * The volume is applied first, so it can never push sound past the ceiling.
 */
export default function SoundSettings() {
    const [master, setMaster] = useState(1);
    const [ceilOn, setCeilOn] = useState(false);
    const [ceilDb, setCeilDb] = useState(-18);
    const [loaded, setLoaded] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const timer = useRef<number | null>(null);

    useEffect(() => {
        api.getAudioLimits()
            .then((l) => {
                setMaster(l.master);
                if (l.max_db < 0) { setCeilOn(true); setCeilDb(l.max_db); }
                setLoaded(true);
            })
            .catch((e) => { setError(String(e)); setLoaded(true); });
        return () => { if (timer.current !== null) window.clearTimeout(timer.current); };
    }, []);

    // Sliders fire many times a second; save once they settle, apply as they move.
    function push(nextMaster: number, nextOn: boolean, nextDb: number) {
        if (timer.current !== null) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(async () => {
            try {
                const applied = await api.setAudioLimits(nextMaster, nextOn ? nextDb : 0);
                window.dispatchEvent(new CustomEvent(AUDIO_LIMITS_CHANGED, { detail: { master: applied.master, maxDb: applied.max_db } }));
                setError(null);
            } catch (e) { setError(String(e)); }
        }, 200);
    }

    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Sound</h2>
            <div className="connect-section">
                <div className="connect-section-title">Volume</div>
                <input type="range" min={0} max={MASTER_MAX} step={0.05} value={master} disabled={!loaded}
                    style={{ width: "100%" }} aria-label="Volume"
                    onChange={(e) => { const v = Number(e.target.value); setMaster(v); push(v, ceilOn, ceilDb); }} />
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>
                    {Math.round(master * 100)}% &mdash; turns everything Farder plays up or down.
                </div>
            </div>

            <div className="connect-section">
                <div className="connect-section-title">Max loudness</div>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                    <input type="checkbox" checked={ceilOn} disabled={!loaded}
                        onChange={(e) => { setCeilOn(e.target.checked); push(master, e.target.checked, ceilDb); }} />
                    <span>Never let anything be louder than this</span>
                </label>
                <input type="range" min={CEIL_MIN} max={CEIL_MAX} step={1} value={ceilDb} disabled={!loaded || !ceilOn}
                    style={{ width: "100%" }} aria-label="Max loudness in decibels"
                    onChange={(e) => { const v = Number(e.target.value); setCeilDb(v); push(master, ceilOn, v); }} />
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>
                    {ceilOn ? describeCeiling(ceilDb) : "Off: sound is as loud as it arrives."}
                </div>
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginTop: 8 }}>
                    Anything that would be louder than this is turned down until it is not &mdash; a
                    shout into a microphone, a clip with the volume cranked, someone&rsquo;s game
                    audio. Quiet sound is left alone. It applies to voice calls, shared-screen audio,
                    and videos and voice messages in chat. It cannot reach sound inside other
                    websites&rsquo; players (YouTube and Spotify), which keep their own volume.
                    Lower numbers are quieter; around &minus;18 dB is a comfortable starting point.
                </div>
            </div>
            {error && <div className="error-text">{error}</div>}
        </div>
    );
}
