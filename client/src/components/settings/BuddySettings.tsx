import { useEffect, useState } from "react";
import * as api from "../../lib/tauri-bridge";
import type { BuddySettings as Buddy } from "../../lib/tauri-bridge";

/**
 * The buddy list: a narrow list of your people and channels in its own small
 * window, like MSN Messenger. Click an entry to open a small chat window
 * for that conversation, one window each.
 */
export default function BuddySettings() {
    const [s, setS] = useState<Buddy | null>(null);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        api.getBuddySettings().then(setS).catch((e) => setError(String(e)));
    }, []);

    if (!s) return <div className="settings-panel"><h2 className="settings-panel-title">Buddy list</h2>{error && <div className="error-text">{error}</div>}</div>;
    const cur = s;

    async function save(next: Buddy) {
        setError(null);
        try { setS(await api.setBuddySettings(next)); } catch (e) { setError(String(e)); }
    }

    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Buddy list</h2>
            <div className="connect-section">
                <div className="connect-section-title">Your people and channels in a small window of their own</div>
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginBottom: 8 }}>
                    Click a name to open a small chat window for it. You can have several open at once
                    and leave the main window closed. Unread messages show as a number beside the name.
                    Encrypted channels can be read there but not answered; use the Farder window for those.
                </div>
                <button className="connect-btn" onClick={() => api.openBuddyList().catch((e) => setError(String(e)))}>
                    Open the buddy list
                </button>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 10 }}>
                    <input type="checkbox" checked={cur.open_at_start} onChange={(e) => void save({ ...cur, open_at_start: e.target.checked })} />
                    <span>Open the buddy list when Farder starts</span>
                </label>
                {error && <div className="error-text" style={{ marginTop: 6 }}>{error}</div>}
            </div>
        </div>
    );
}
