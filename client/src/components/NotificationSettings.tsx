import { useState, useEffect, useMemo } from "react";
import * as api from "../lib/tauri-bridge";
import type { NotificationPrefs } from "../lib/tauri-bridge";
import { useApp } from "../context/ServerContext";
import { refreshNotifPrefsCache } from "../hooks/useServerEvents";
import { publicKeyToString } from "../lib/types";

interface Props { onClose: () => void; }

export default function NotificationSettings({ onClose }: Props) {
    const { state } = useApp();
    const [prefs, setPrefs] = useState<NotificationPrefs | null>(null);
    const [newKeyword, setNewKeyword] = useState("");
    const [saved, setSaved] = useState(false);
    const [userSearch, setUserSearch] = useState("");

    // Collect unique members across all connected servers
    const allMembers = useMemo(() => {
        const seen = new Set<string>();
        const result: { pk: string; displayName: string }[] = [];
        for (const serverId in state.servers) {
            for (const m of state.servers[serverId].members) {
                const pk = publicKeyToString(m.public_key);
                if (!seen.has(pk)) {
                    seen.add(pk);
                    result.push({ pk, displayName: m.display_name });
                }
            }
        }
        return result.sort((a, b) => a.displayName.localeCompare(b.displayName));
    }, [state.servers]);

    useEffect(() => {
        api.getNotificationPrefs().then(setPrefs);
    }, []);

    async function save(updated: NotificationPrefs) {
        setPrefs(updated);
        await api.saveNotificationPrefs(updated);
        // Refresh the module-level cache in useServerEvents
        refreshNotifPrefsCache();
        setSaved(true);
        setTimeout(() => setSaved(false), 2000);
    }

    if (!prefs) return null;

    return (
        <div className="modal-overlay" onClick={onClose}>
            <div className="modal-dialog" onClick={e => e.stopPropagation()} style={{ minWidth: 400, maxHeight: "80vh", display: "flex", flexDirection: "column" }}>
                <div className="modal-titlebar">
                    <span>Notification Settings</span>
                    <button className="modal-close" onClick={onClose}>X</button>
                </div>
                <div className="modal-body" style={{ overflowY: "auto", flex: 1 }}>

                    {/* DM Notifications */}
                    <div className="connect-section">
                        <div className="connect-section-title">Direct Messages</div>
                        <select className="connect-input" value={prefs.dmNotifications}
                            onChange={e => save({ ...prefs, dmNotifications: e.target.value as NotificationPrefs["dmNotifications"] })}>
                            <option value="all">All DMs</option>
                            <option value="specific">Specific users only</option>
                            <option value="none">None</option>
                        </select>
                        {prefs.dmNotifications === "specific" && (
                            <div style={{ marginTop: 8 }}>
                                <input className="connect-input" placeholder="Search users..." value={userSearch}
                                    onChange={e => setUserSearch(e.target.value)} style={{ marginBottom: 6 }} />
                                <div style={{ maxHeight: 150, overflowY: "auto", border: "1px solid var(--xp-border)", borderRadius: 2 }}>
                                    {allMembers
                                        .filter(m => m.displayName.toLowerCase().includes(userSearch.toLowerCase()))
                                        .map(m => {
                                            const isAllowed = prefs.dmAllowedUsers.includes(m.pk);
                                            return (
                                                <div key={m.pk} style={{ display: "flex", alignItems: "center", gap: 6, padding: "3px 8px", fontSize: 11, cursor: "pointer",
                                                    background: isAllowed ? "rgba(49,105,198,0.1)" : undefined }}
                                                    onClick={() => {
                                                        const updated = isAllowed
                                                            ? prefs.dmAllowedUsers.filter(k => k !== m.pk)
                                                            : [...prefs.dmAllowedUsers, m.pk];
                                                        save({ ...prefs, dmAllowedUsers: updated });
                                                    }}>
                                                    <span style={{ width: 16, textAlign: "center" }}>{isAllowed ? "+" : ""}</span>
                                                    <span>{m.displayName}</span>
                                                </div>
                                            );
                                        })}
                                </div>
                            </div>
                        )}
                    </div>

                    {/* Per-Server Settings */}
                    <div className="connect-section">
                        <div className="connect-section-title">Server Notifications</div>
                        {state.serverList.map(s => {
                            const serverPref = prefs.servers[s.id]?.mode ?? "all";
                            return (
                                <div key={s.id} style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 4 }}>
                                    <span style={{ fontSize: 11 }}>{s.name}</span>
                                    <select className="connect-input" style={{ width: 120 }} value={serverPref}
                                        onChange={e => save({
                                            ...prefs,
                                            servers: { ...prefs.servers, [s.id]: { mode: e.target.value as "all" | "mentions" | "none" } }
                                        })}>
                                        <option value="all">All messages</option>
                                        <option value="mentions">Mentions only</option>
                                        <option value="none">None</option>
                                    </select>
                                </div>
                            );
                        })}
                    </div>

                    {/* Keywords */}
                    <div className="connect-section">
                        <div className="connect-section-title">Notification Keywords</div>
                        <p style={{ fontSize: 10, color: "#666", marginBottom: 4 }}>Get notified when these words appear in any message</p>
                        <div style={{ display: "flex", flexWrap: "wrap", gap: 4, marginBottom: 6 }}>
                            {prefs.keywords.map((kw, i) => (
                                <span key={i} className="profile-card-role" style={{ cursor: "pointer" }}
                                    onClick={() => save({ ...prefs, keywords: prefs.keywords.filter((_, j) => j !== i) })}>
                                    {kw} x
                                </span>
                            ))}
                        </div>
                        <div style={{ display: "flex", gap: 4 }}>
                            <input className="connect-input" value={newKeyword} onChange={e => setNewKeyword(e.target.value)}
                                placeholder="Add keyword..." onKeyDown={e => {
                                    if (e.key === "Enter" && newKeyword.trim()) {
                                        save({ ...prefs, keywords: [...prefs.keywords, newKeyword.trim()] });
                                        setNewKeyword("");
                                    }
                                }} />
                            <button className="xp-button" onClick={() => {
                                if (newKeyword.trim()) {
                                    save({ ...prefs, keywords: [...prefs.keywords, newKeyword.trim()] });
                                    setNewKeyword("");
                                }
                            }}>Add</button>
                        </div>
                    </div>

                    {/* Mention notifications */}
                    <div className="connect-section">
                        <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11 }}>
                            <input type="checkbox" checked={prefs.mentionNotifications}
                                onChange={e => save({ ...prefs, mentionNotifications: e.target.checked })} />
                            Notify on @mentions
                        </label>
                    </div>

                    {/* Custom Event Triggers */}
                    <div className="connect-section">
                        <div className="connect-section-title">Event Notifications</div>
                        <p style={{ fontSize: 10, color: "#666", marginBottom: 6 }}>Get notified when these events happen</p>

                        <label className="notif-toggle">
                            <input type="checkbox" checked={prefs.notifyOnMemberJoin ?? false}
                                onChange={e => save({ ...prefs, notifyOnMemberJoin: e.target.checked })} />
                            New member joins a server
                        </label>

                        <label className="notif-toggle">
                            <input type="checkbox" checked={prefs.notifyOnMemberLeave ?? false}
                                onChange={e => save({ ...prefs, notifyOnMemberLeave: e.target.checked })} />
                            Member leaves a server
                        </label>

                        <label className="notif-toggle">
                            <input type="checkbox" checked={prefs.notifyOnReaction ?? false}
                                onChange={e => save({ ...prefs, notifyOnReaction: e.target.checked })} />
                            Someone reacts to your message
                        </label>

                        <label className="notif-toggle">
                            <input type="checkbox" checked={prefs.notifyOnThreadReply ?? false}
                                onChange={e => save({ ...prefs, notifyOnThreadReply: e.target.checked })} />
                            New reply in a thread you participated in
                        </label>
                    </div>

                    {saved && <div className="success-text">Settings saved!</div>}
                </div>
            </div>
        </div>
    );
}
