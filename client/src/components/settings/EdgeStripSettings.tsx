import { useEffect, useState } from "react";
import * as api from "../../lib/tauri-bridge";
import type { EdgeStripSettings as Strip } from "../../lib/tauri-bridge";
import { refreshStripCache } from "../../hooks/useServerEvents";
import RadioOption from "./RadioOption";

const WIDTHS: { label: string; value: number }[] = [
    { label: "Narrow", value: 320 },
    { label: "Medium", value: 460 },
    { label: "Wide", value: 640 },
];

/**
 * The edge strip: a one-line bar docked to the top or bottom of the screen, for
 * leaving Farder open beside a game or a document. It shows who has written to you
 * and lets you reply without opening the app. It replaces the pop-up while it is on,
 * and says no more than "What a pop-up may show" allows.
 */
export default function EdgeStripSettings() {
    const [s, setS] = useState<Strip | null>(null);
    const [popupsOff, setPopupsOff] = useState(false);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        api.getEdgeStrip().then(setS).catch((e) => setError(String(e)));
        api.getNotificationPrefs().then((p) => setPopupsOff(p.popupStyle === "off")).catch(() => {});
    }, []);

    if (!s) return <div className="settings-panel"><h2 className="settings-panel-title">Edge strip</h2>{error && <div className="error-text">{error}</div>}</div>;

    async function save(next: Strip) {
        setError(null);
        try {
            setS(await api.setEdgeStrip(next));
            refreshStripCache();
        } catch (e) { setError(String(e)); }
    }
    const cur = s;
    const off = !cur.enabled;

    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Edge strip</h2>
            <div className="connect-section">
                <div className="connect-section-title">A one-line bar for messages while you do something else</div>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                    <input type="checkbox" checked={cur.enabled} onChange={(e) => void save({ ...cur, enabled: e.target.checked })} />
                    <span>Show the edge strip</span>
                </label>
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>
                    It shows who has written to you and a one-line preview with a Reply box. It stays out of the way
                    while Farder is in front, and it never takes your keyboard until you press Reply.
                    While it is on it replaces the pop-up at the top of the screen.
                </div>
                {popupsOff && cur.enabled && (
                    <div className="error-text" style={{ marginTop: 6 }}>
                        Pop-ups are set to "No pop-ups at all" (Notifications), so the strip stays empty. Choose another pop-up style there.
                    </div>
                )}
            </div>
            <div className="connect-section" style={{ opacity: off ? 0.5 : 1 }}>
                <div className="connect-section-title">Where it sits</div>
                <RadioOption selected={cur.dock === "top"} label="Top of the screen" onSelect={() => void save({ ...cur, dock: "top" })} />
                <RadioOption selected={cur.dock === "bottom"} label="Bottom of the screen" description="Sits above the taskbar." onSelect={() => void save({ ...cur, dock: "bottom" })} />
                <div className="connect-section-title" style={{ marginTop: 10 }}>Which part of the edge</div>
                <RadioOption selected={cur.align === "left"} label="Left" onSelect={() => void save({ ...cur, align: "left" })} />
                <RadioOption selected={cur.align === "center"} label="Middle" onSelect={() => void save({ ...cur, align: "center" })} />
                <RadioOption selected={cur.align === "right"} label="Right" onSelect={() => void save({ ...cur, align: "right" })} />
            </div>
            <div className="connect-section" style={{ opacity: off ? 0.5 : 1 }}>
                <div className="connect-section-title">How wide</div>
                {WIDTHS.map((w) => (
                    <RadioOption key={w.value} selected={cur.width === w.value} label={`${w.label} (${w.value} px)`} onSelect={() => void save({ ...cur, width: w.value })} />
                ))}
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10 }}>
                    <input type="checkbox" checked={cur.always_visible} onChange={(e) => void save({ ...cur, always_visible: e.target.checked })} />
                    <span>Keep it on screen even when nothing is unread</span>
                </label>
            </div>
            {error && <div className="error-text">{error}</div>}
        </div>
    );
}
