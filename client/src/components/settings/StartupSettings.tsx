import { useEffect, useState } from "react";
import * as api from "../../lib/tauri-bridge";
import type { WindowBehavior } from "../../lib/tauri-bridge";

/**
 * Leave Farder running like a messenger: closing the window keeps it in the tray
 * (so messages still arrive and pop up), it can start in the tray, and it can
 * start when you sign in to Windows. Quit is always in the tray menu.
 */
export default function StartupSettings() {
    const [b, setB] = useState<WindowBehavior | null>(null);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => { api.getWindowBehavior().then(setB).catch((e) => setError(String(e))); }, []);
    if (!b) return <div className="settings-panel"><h2 className="settings-panel-title">Startup &amp; tray</h2>{error && <div className="error-text">{error}</div>}</div>;

    async function saveBehavior(closeToTray: boolean, startMinimized: boolean) {
        setError(null);
        try { await api.setWindowBehavior(closeToTray, startMinimized); setB({ ...b!, close_to_tray: closeToTray, start_minimized: startMinimized }); }
        catch (e) { setError(String(e)); }
    }
    async function saveAutostart(enabled: boolean) {
        setError(null);
        try { const now = await api.setAutostart(enabled); setB({ ...b!, autostart: now }); }
        catch (e) { setError(String(e)); }
    }

    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Startup &amp; tray</h2>
            <div className="connect-section">
                <div className="connect-section-title">Keep Farder running</div>
                {!b.tray_available && (
                    <div className="error-text" style={{ marginBottom: 8 }}>
                        The system tray is not available, so closing the window quits Farder and it cannot start hidden.
                    </div>
                )}
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                    <input type="checkbox" checked={b.close_to_tray} disabled={!b.tray_available}
                        onChange={(e) => void saveBehavior(e.target.checked, b.start_minimized)} />
                    <span>Closing the window keeps Farder running in the tray</span>
                </label>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                    <input type="checkbox" checked={b.start_minimized} disabled={!b.tray_available}
                        onChange={(e) => void saveBehavior(b.close_to_tray, e.target.checked)} />
                    <span>Start in the tray instead of opening the window</span>
                </label>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6, opacity: b.autostart_unavailable ? 0.6 : 1 }}>
                    <input type="checkbox" checked={b.autostart} disabled={!!b.autostart_unavailable}
                        onChange={(e) => void saveAutostart(e.target.checked)} />
                    <span>Start Farder when I sign in to Windows (in the tray)</span>
                </label>
                {b.autostart_unavailable && (
                    <div style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>{b.autostart_unavailable}</div>
                )}
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginTop: 8 }}>
                    Click the tray icon to open Farder; right-click it for Open / Quit. While it is in the
                    tray, messages still raise pop-ups (see Notifications), so you can reply without opening
                    the window. &ldquo;Sign in&rdquo; adds one entry to your own Windows startup list and turning it off removes it.
                </div>
            </div>
            {error && <div className="error-text">{error}</div>}
        </div>
    );
}
