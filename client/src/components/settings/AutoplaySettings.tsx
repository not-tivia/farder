import { useState } from "react";
import { useActiveServer } from "../../context/ServerContext";
import { useDataSaver } from "../../context/DataSaverContext";
import { useAutoplaySettings } from "../../hooks/useAutoplay";
import { setAutoplay, setTrusted, MAX_TRUSTED } from "../../lib/autoplay";
import { publicKeyToString } from "../../lib/types";

/**
 * Autoplay: video from people you trust starts by itself, muted, in chat and in
 * the pop-up at the top of the screen. OFF unless you turn it on, and even then
 * only for the people on this list. It is fetched through the relay like any
 * other media, so the sender never learns your address.
 */
export default function AutoplaySettings() {
    const s = useAutoplaySettings();
    const server = useActiveServer();
    const { settings: dataSaver } = useDataSaver();
    const [pick, setPick] = useState("");

    const members = server?.members ?? [];
    const nameOf = (pk: string) =>
        members.find((m) => publicKeyToString(m.public_key) === pk)?.display_name ?? `${pk.slice(0, 12)}...`;
    const candidates = members.filter((m) => !m.is_bot && !s.trusted.includes(publicKeyToString(m.public_key)));

    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Autoplay</h2>
            <div className="connect-section">
                <div className="connect-section-title">Let trusted people's media play by itself</div>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                    <input type="checkbox" checked={s.enabled} onChange={(e) => setAutoplay({ ...s, enabled: e.target.checked })} />
                    <span>Autoplay media from the people below</span>
                </label>
                <label style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6, opacity: s.enabled ? 1 : 0.5 }}>
                    <input type="checkbox" checked={s.inPopups} disabled={!s.enabled}
                        onChange={(e) => setAutoplay({ ...s, inPopups: e.target.checked })} />
                    <span>Also show it in the pop-up at the top of the screen</span>
                </label>
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>
                    Videos start muted, only when they are on screen, and pause when you scroll away.
                    Clicking one hands it back to you. Nothing autoplays for anyone not on this list, in
                    pop-ups only direct video or image links are shown, and the system's reduce-motion
                    setting always wins.
                    {dataSaver.enabled && " Data Saver is on right now, so nothing will autoplay until it is off."}
                </div>
            </div>

            <div className="connect-section">
                <div className="connect-section-title">People I trust ({s.trusted.length})</div>
                {s.trusted.length === 0 && (
                    <div style={{ fontSize: 11, color: "var(--xp-text-muted)" }}>Nobody yet.</div>
                )}
                {s.trusted.map((pk) => (
                    <div key={pk} style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                        <span style={{ flex: 1 }}>{nameOf(pk)}</span>
                        <button className="xp-button" onClick={() => setAutoplay(setTrusted(s, pk, false))}>Remove</button>
                    </div>
                ))}
                {server && (
                    <div style={{ display: "flex", gap: 6, marginTop: 8 }}>
                        <select className="connect-input" value={pick} onChange={(e) => setPick(e.target.value)}>
                            <option value="">Add someone from {server.serverName}...</option>
                            {candidates.map((m) => {
                                const pk = publicKeyToString(m.public_key);
                                return <option key={pk} value={pk}>{m.display_name}</option>;
                            })}
                        </select>
                        <button className="xp-button" disabled={!pick || s.trusted.length >= MAX_TRUSTED}
                            onClick={() => { setAutoplay(setTrusted(s, pick, true)); setPick(""); }}>Add</button>
                    </div>
                )}
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)", marginTop: 6 }}>
                    You can also right-click anyone in the member list and choose &ldquo;Autoplay their media&rdquo;.
                </div>
            </div>
        </div>
    );
}
