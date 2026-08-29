import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import { E2eeConfirmDialog } from "./E2eeConfirmDialog";
import type { ChannelInfo } from "../lib/types";
import BannedMembersTab from "./BannedMembersTab";
import AuditLogTab from "./AuditLogTab";
import BotsTab from "./BotsTab";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";

// Module-level cache for own public key (same pattern as MemberSidebar)
let cachedOwnPk: string | null = null;

interface Props { onClose: () => void; }

export default function ServerSettingsDialog({ onClose }: Props) {
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const [newChName, setNewChName] = useState("");
  const [newChType, setNewChType] = useState("Text");
  const [newChCatId, setNewChCatId] = useState<number | undefined>(undefined);
  const [newChE2ee, setNewChE2ee] = useState(false);
  const [newCatName, setNewCatName] = useState("");
  const [newRoleName, setNewRoleName] = useState("");
  const [newRoleColor, setNewRoleColor] = useState("#3169C6");
  const [error, setError] = useState<string | null>(null);
  const [serverAvatarUrl, setServerAvatarUrl] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<"general" | "banned" | "audit" | "bots">("general");
  // -- Retire this device (sub-5b G4) -------------------------------------
  const logServerId = activeServer?.logServerId ?? null;
  const [showRetire, setShowRetire] = useState(false);
  const [retireBusy, setRetireBusy] = useState(false);
  const [retireError, setRetireError] = useState<string | null>(null);
  const [retireNote, setRetireNote] = useState<string | null>(null);

  async function retireThisDevice() {
    if (!serverId || !logServerId) return;
    setRetireBusy(true);
    setRetireError(null);
    try {
      await api.revokeOwnDevice(serverId, logServerId);
      setRetireNote("This device has been retired. It can no longer read new encrypted messages on this server.");
      setShowRetire(false);
    } catch (e) {
      setRetireError(String(e));
    } finally {
      setRetireBusy(false);
    }
  }
  const [ownPk, setOwnPk] = useState(cachedOwnPk);

  useEffect(() => {
    if (!cachedOwnPk) {
      api.getPublicKey().then(pk => { cachedOwnPk = pk; setOwnPk(pk); });
    }
  }, []);

  const members = activeServer?.members ?? [];
  const roles = activeServer?.roles ?? [];
  const { bits } = ownPk
    ? getActorPermissions(members, roles, ownPk, activeServer?.ownerPublicKey ?? null)
    : { bits: 0n };
  const canBan = hasPermission(bits, PERMISSIONS.BAN_MEMBERS);
  const canManageServer = hasPermission(bits, PERMISSIONS.MANAGE_SERVER);

  useEffect(() => {
    if (serverId) {
      api.getServerAvatar(serverId).then(url => setServerAvatarUrl(url));
    }
  }, [serverId]);

  async function handleChangeServerIcon() {
    if (!serverId) return;
    const path = await api.pickFile();
    if (path) {
      try {
        const url = await api.setServerAvatar(serverId, path);
        setServerAvatarUrl(url);
      } catch (e) { setError(String(e)); }
    }
  }

  async function handleRemoveServerIcon() {
    if (!serverId) return;
    setServerAvatarUrl(null);
  }

  const categories = activeServer?.categories ?? [];
  const channels = activeServer?.channels ?? [];

  const sortedCategories = [...categories].sort((a, b) => a.position - b.position);
  const allChannels = channels.filter(c => c.channel_type !== "Thread");
  const uncategorized = allChannels.filter(c => c.category_id === null).sort((a, b) => a.position - b.position);

  function channelsInCategory(catId: number) {
    return allChannels.filter(c => c.category_id === catId).sort((a, b) => a.position - b.position);
  }

  async function swapChannels(chList: ChannelInfo[], index: number, direction: -1 | 1) {
    if (!serverId) return;
    const target = index + direction;
    if (target < 0 || target >= chList.length) return;
    try {
      for (let i = 0; i < chList.length; i++) {
        if (chList[i].position !== i) {
          await api.updateChannel(serverId, chList[i].id, { position: i });
        }
      }
      await api.updateChannel(serverId, chList[index].id, { position: target });
      await api.updateChannel(serverId, chList[target].id, { position: index });
    } catch (e) { setError(String(e)); }
  }

  async function swapCategories(index: number, direction: -1 | 1) {
    if (!serverId) return;
    const target = index + direction;
    if (target < 0 || target >= sortedCategories.length) return;
    try {
      for (let i = 0; i < sortedCategories.length; i++) {
        if (sortedCategories[i].position !== i) {
          await api.updateCategory(serverId, sortedCategories[i].id, { position: i });
        }
      }
      await api.updateCategory(serverId, sortedCategories[index].id, { position: target });
      await api.updateCategory(serverId, sortedCategories[target].id, { position: index });
    } catch (e) { setError(String(e)); }
  }

  async function moveChannelToCategory(channelId: number, categoryId: number | null) {
    if (!serverId) return;
    try {
      await api.updateChannel(serverId, channelId, { categoryId, position: 0 });
    } catch (e) { setError(String(e)); }
  }

  async function handleCreateChannel() {
    if (!newChName.trim() || !serverId) return;
    try {
      if (newChE2ee) {
        const logServerId = activeServer?.logServerId ?? null;
        if (!logServerId) { setError("This server has no event log"); return; }
        await api.createE2eeChannel(serverId, logServerId, newChName.trim());
      } else {
        await api.createChannel(serverId, newChName.trim(), newChType, newChCatId);
      }
      setNewChName("");
      setNewChE2ee(false);
    } catch (e) { setError(String(e)); }
  }

  async function handleCreateCategory() {
    if (!newCatName.trim() || !serverId) return;
    try {
      await api.createCategory(serverId, newCatName.trim());
      setNewCatName("");
    } catch (e) { setError(String(e)); }
  }

  async function handleCreateRole() {
    if (!newRoleName.trim() || !serverId) return;
    try {
      await api.createRole(serverId, newRoleName.trim(), 0x2007, newRoleColor);
      setNewRoleName("");
    } catch (e) { setError(String(e)); }
  }

  function renderChannelRow(ch: ChannelInfo, siblings: ChannelInfo[], index: number) {
    const prefix = ch.channel_type === "Announcement" ? "!" : ch.channel_type === "Voice" ? "~" : "#";
    return (
      <div key={ch.id} className="organizer-row organizer-channel">
        <span className="organizer-name">{prefix} {ch.name}</span>
        <div className="organizer-actions">
          <button className="organizer-btn" disabled={index === 0} onClick={() => swapChannels(siblings, index, -1)} title="Move up">^</button>
          <button className="organizer-btn" disabled={index === siblings.length - 1} onClick={() => swapChannels(siblings, index, 1)} title="Move down">v</button>
          <select className="organizer-move" value="" onChange={(e) => {
            if (e.target.value === "__none__") moveChannelToCategory(ch.id, null);
            else if (e.target.value) moveChannelToCategory(ch.id, Number(e.target.value));
          }}>
            <option value="">Move...</option>
            {ch.category_id !== null && <option value="__none__">Uncategorized</option>}
            {sortedCategories.filter(c => c.id !== ch.category_id).map(c => (
              <option key={c.id} value={c.id}>{c.name}</option>
            ))}
          </select>
          <button className="organizer-btn organizer-delete" onClick={async () => {
            if (serverId) try { await api.deleteChannel(serverId, ch.id); } catch {}
          }} title="Delete">x</button>
        </div>
      </div>
    );
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()} style={{ minWidth: 480, maxHeight: "80vh", display: "flex", flexDirection: "column" }}>
        <div className="modal-titlebar">
          <span>Server Settings</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        {/* Tab bar */}
        <div style={{ display: "flex", borderBottom: "1px solid var(--xp-border)", padding: "0 12px", gap: 4 }}>
          <button
            className={`tab-btn${activeTab === "general" ? " tab-btn--active" : ""}`}
            onClick={() => setActiveTab("general")}
          >
            General
          </button>
          {canBan && (
            <button
              className={`tab-btn${activeTab === "banned" ? " tab-btn--active" : ""}`}
              onClick={() => setActiveTab("banned")}
            >
              Banned Members
            </button>
          )}
          {canManageServer && (
            <button
              className={`tab-btn${activeTab === "audit" ? " tab-btn--active" : ""}`}
              onClick={() => setActiveTab("audit")}
            >
              Audit Log
            </button>
          )}
          {canManageServer && (
            <button
              className={`tab-btn${activeTab === "bots" ? " tab-btn--active" : ""}`}
              onClick={() => setActiveTab("bots")}
            >
              Bots
            </button>
          )}
        </div>

        <div className="modal-body" style={{ overflowY: "auto", flex: 1 }}>
          {activeTab === "banned" && serverId && (
            <BannedMembersTab serverId={serverId} />
          )}
          {activeTab === "audit" && serverId && (
            <AuditLogTab serverId={serverId} />
          )}
          {activeTab === "bots" && serverId && (
            <BotsTab serverId={serverId} />
          )}
          {activeTab === "general" && <>
            {/* Encryption & devices (sub-5b G4). Per-server, because device
                revocation is recorded in THAT server's log. */}
            {logServerId && (
              <div className="organizer-create" style={{ marginTop: 8, marginBottom: 16, borderBottom: "1px solid var(--xp-border)", paddingBottom: 12 }}>
                <label className="connect-label">🔒 Encryption &amp; devices</label>
                <div style={{ fontSize: 11, color: "var(--xp-text-muted)", margin: "4px 0 8px", lineHeight: 1.5 }}>
                  Encrypted channels are read by <em>devices</em>, not accounts. If you lose a
                  machine, retire its access here — your account and your other devices are
                  unaffected.
                </div>
                {retireNote && <div className="success-text" style={{ marginBottom: 8 }}>{retireNote}</div>}
                <button className="xp-button" onClick={() => { setRetireNote(null); setShowRetire(true); }}>
                  Retire this device…
                </button>
              </div>
            )}

          {error && <div className="error-text" style={{ marginBottom: 8 }}>{error}</div>}

          {/* Server Icon */}
          <div className="organizer-create" style={{ marginBottom: 12 }}>
            <div className="connect-section-title">Server Icon</div>
            <div style={{ display: "flex", alignItems: "center", gap: 12, marginTop: 8 }}>
              <div className="server-avatar-preview">
                {serverAvatarUrl ? (
                  <img src={serverAvatarUrl} alt="Server icon" style={{ width: "100%", height: "100%", borderRadius: 6, objectFit: "cover" }} />
                ) : (
                  <span>{activeServer?.serverName?.charAt(0)?.toUpperCase() ?? "?"}</span>
                )}
              </div>
              <div>
                <button className="xp-button" onClick={handleChangeServerIcon}>Change Icon</button>
                {serverAvatarUrl && (
                  <button className="xp-button" onClick={handleRemoveServerIcon} style={{ marginLeft: 4 }}>Remove</button>
                )}
              </div>
            </div>
          </div>

          <div className="organizer-section">
            {uncategorized.length > 0 && (
              <div className="organizer-group">
                <div className="organizer-group-header">Uncategorized</div>
                {uncategorized.map((ch, i) => renderChannelRow(ch, uncategorized, i))}
              </div>
            )}

            {sortedCategories.map((cat, catIdx) => {
              const catChannels = channelsInCategory(cat.id);
              return (
                <div key={cat.id} className="organizer-group">
                  <div className="organizer-row organizer-category-row">
                    <span className="organizer-category-name">{cat.name}</span>
                    <div className="organizer-actions">
                      <button className="organizer-btn" disabled={catIdx === 0} onClick={() => swapCategories(catIdx, -1)} title="Move up">^</button>
                      <button className="organizer-btn" disabled={catIdx === sortedCategories.length - 1} onClick={() => swapCategories(catIdx, 1)} title="Move down">v</button>
                      <button className="organizer-btn organizer-delete" onClick={async () => {
                        if (serverId) try { await api.deleteCategory(serverId, cat.id); } catch {}
                      }} title="Delete">x</button>
                    </div>
                  </div>
                  {catChannels.map((ch, i) => renderChannelRow(ch, catChannels, i))}
                  {catChannels.length === 0 && (
                    <div className="organizer-empty">No channels</div>
                  )}
                </div>
              );
            })}
          </div>

          <div className="organizer-create" style={{ marginTop: 16, borderTop: "1px solid var(--xp-border)", paddingTop: 12 }}>
            <div style={{ display: "flex", gap: 6, marginBottom: 8, alignItems: "flex-end" }}>
              <div style={{ flex: 1 }}>
                <label className="connect-label">New Channel</label>
                <input className="connect-input" value={newChName} onChange={(e) => setNewChName(e.target.value)} placeholder="Channel name" />
              </div>
              <select className="connect-input" style={{ width: 100 }} value={newChType} disabled={newChE2ee} title={newChE2ee ? "Encrypted channels are text-only" : undefined} onChange={(e) => setNewChType(e.target.value)}>
                <option value="Text">Text</option>
                <option value="Announcement">Announce</option>
                <option value="Voice">Voice</option>
              </select>
              <select className="connect-input" style={{ width: 120 }} value={newChCatId ?? ""} disabled={newChE2ee} title={newChE2ee ? "Encrypted channels can't be categorized yet" : undefined} onChange={(e) => setNewChCatId(e.target.value ? Number(e.target.value) : undefined)}>
                <option value="">No category</option>
                {sortedCategories.map(c => <option key={c.id} value={c.id}>{c.name}</option>)}
              </select>
              <label className="connect-label" style={{ display: "flex", alignItems: "center", gap: 4, margin: 0, whiteSpace: "nowrap" }} title="End-to-end encrypted — the server can't read messages">
                <input type="checkbox" checked={newChE2ee} onChange={(e) => setNewChE2ee(e.target.checked)} />
                Encrypted
              </label>
              <button className="xp-button" onClick={handleCreateChannel} disabled={!newChName.trim()}>Add</button>
            </div>
            {/* The owner's 2026-08-28 decision: a server is NOT fully encrypted;
                specific channels are. That is only defensible if the difference is
                legible AT THE POINT OF CHOICE, so this explains whichever option is
                currently selected — including what a normal channel already
                protects, which the old copy never said. */}
            <div className="channel-class-explainer" style={{ marginBottom: 8 }}>
              {newChE2ee ? (
                <>
                  <strong>Encrypted channel.</strong> Only members can read messages — the server
                  host cannot, even though they run it.
                  <br />
                  <span className="channel-class-cost">
                    Polls, reactions, threads, bots, link previews and server-side search do not
                    work in encrypted channels.
                  </span>
                </>
              ) : (
                <>
                  <strong>Normal channel.</strong> Messages are encrypted in transit and the
                  server is yours, but whoever runs it can read them.
                  <br />
                  <span className="channel-class-cost">
                    Everything works: polls, reactions, threads, bots, link previews and search.
                  </span>
                </>
              )}
            </div>
            <div style={{ display: "flex", gap: 6, alignItems: "flex-end" }}>
              <div style={{ flex: 1 }}>
                <label className="connect-label">New Category</label>
                <input className="connect-input" value={newCatName} onChange={(e) => setNewCatName(e.target.value)} placeholder="Category name" />
              </div>
              <button className="xp-button" onClick={handleCreateCategory} disabled={!newCatName.trim()}>Add</button>
            </div>
          </div>

          {/* Roles Section */}
          <div className="organizer-create" style={{ marginTop: 16, borderTop: "1px solid var(--xp-border)", paddingTop: 12 }}>
            <div className="connect-section-title" style={{ marginBottom: 8 }}>Roles</div>

            {/* Existing roles — sorted by position descending (highest rank at top) */}
            {(() => {
              const sortedRoles = (activeServer?.roles ?? [])
                .filter(r => r.name !== "@everyone")
                .sort((a, b) => b.position - a.position);
              return sortedRoles.map((r, index) => (
                <div key={r.id} className="organizer-row">
                  <span className="organizer-name" style={{ color: r.color ?? undefined }}>
                    {r.name}
                  </span>
                  <div className="organizer-actions">
                    <label className="connect-label" style={{ display: "flex", alignItems: "center", gap: 4, margin: 0, whiteSpace: "nowrap" }}>
                      <input
                        type="checkbox"
                        checked={r.hoist}
                        onChange={async (e) => {
                          if (serverId) try { await api.updateRole(serverId, r.id, { hoist: e.target.checked }); } catch (e) { setError(String(e)); }
                        }}
                      />
                      Display separately
                    </label>
                    <button
                      className="organizer-btn"
                      disabled={index === 0}
                      onClick={async () => {
                        if (!serverId) return;
                        try {
                          const newOrder = [...sortedRoles];
                          [newOrder[index], newOrder[index - 1]] = [newOrder[index - 1], newOrder[index]];
                          for (let j = 0; j < newOrder.length; j++) {
                            const newPosition = (newOrder.length - 1) - j;
                            if (newOrder[j].position !== newPosition) {
                              await api.updateRole(serverId, newOrder[j].id, { position: newPosition });
                            }
                          }
                        } catch (e) { setError(String(e)); }
                      }}
                      title="Move up"
                    >^</button>
                    <button
                      className="organizer-btn"
                      disabled={index === sortedRoles.length - 1}
                      onClick={async () => {
                        if (!serverId) return;
                        try {
                          const newOrder = [...sortedRoles];
                          [newOrder[index], newOrder[index + 1]] = [newOrder[index + 1], newOrder[index]];
                          for (let j = 0; j < newOrder.length; j++) {
                            const newPosition = (newOrder.length - 1) - j;
                            if (newOrder[j].position !== newPosition) {
                              await api.updateRole(serverId, newOrder[j].id, { position: newPosition });
                            }
                          }
                        } catch (e) { setError(String(e)); }
                      }}
                      title="Move down"
                    >v</button>
                    <button className="organizer-btn organizer-delete" onClick={async () => {
                      if (serverId) try { await api.deleteRole(serverId, r.id); } catch {}
                    }} title="Delete">x</button>
                  </div>
                </div>
              ));
            })()}

            {/* Create new role */}
            <div style={{ display: "flex", gap: 6, marginTop: 8, alignItems: "flex-end" }}>
              <div style={{ flex: 1 }}>
                <label className="connect-label">New Role</label>
                <input className="connect-input" value={newRoleName} onChange={(e) => setNewRoleName(e.target.value)} placeholder="Role name" />
              </div>
              <div>
                <label className="connect-label">Color</label>
                <input type="color" value={newRoleColor} onChange={(e) => setNewRoleColor(e.target.value)} style={{ width: 30, height: 24, padding: 0, border: "1px solid var(--xp-border)" }} />
              </div>
              <button className="xp-button" onClick={handleCreateRole} disabled={!newRoleName.trim()}>Add</button>
            </div>
          </div>
          </>}
        </div>
      </div>

      {showRetire && (
        <E2eeConfirmDialog
          title="Retire this device?"
          consequence={
            <>
              <strong>This cannot be undone.</strong> This device will no longer be able to read
              new messages in encrypted channels on this server, and it can never be re-enabled —
              a retired device stays retired permanently.
              <br /><br />
              Your account is <em>not</em> affected: your other devices keep working, and you can
              set this machine up again as a new device.
              <br /><br />
              Encrypted messages already saved on this machine stay readable here.
            </>
          }
          confirmLabel="Retire this device"
          busy={retireBusy}
          error={retireError}
          onCancel={() => setShowRetire(false)}
          onConfirm={retireThisDevice}
        />
      )}
    </div>
  );
}
