import { useState } from "react";
import * as api from "../lib/tauri-bridge";
import { useActiveServer, useActiveServerId } from "../context/ServerContext";
import type { ChannelInfo } from "../lib/types";

interface Props { onClose: () => void; }

export default function ServerSettingsDialog({ onClose }: Props) {
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const [newChName, setNewChName] = useState("");
  const [newChType, setNewChType] = useState("Text");
  const [newChCatId, setNewChCatId] = useState<number | undefined>(undefined);
  const [newCatName, setNewCatName] = useState("");
  const [newRoleName, setNewRoleName] = useState("");
  const [newRoleColor, setNewRoleColor] = useState("#3169C6");
  const [error, setError] = useState<string | null>(null);

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
      await api.createChannel(serverId, newChName.trim(), newChType, newChCatId);
      setNewChName("");
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
    const prefix = ch.channel_type === "Announcement" ? "!" : "#";
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
        <div className="modal-body" style={{ overflowY: "auto", flex: 1 }}>
          {error && <div className="error-text" style={{ marginBottom: 8 }}>{error}</div>}

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
              <select className="connect-input" style={{ width: 100 }} value={newChType} onChange={(e) => setNewChType(e.target.value)}>
                <option value="Text">Text</option>
                <option value="Announcement">Announce</option>
              </select>
              <select className="connect-input" style={{ width: 120 }} value={newChCatId ?? ""} onChange={(e) => setNewChCatId(e.target.value ? Number(e.target.value) : undefined)}>
                <option value="">No category</option>
                {sortedCategories.map(c => <option key={c.id} value={c.id}>{c.name}</option>)}
              </select>
              <button className="xp-button" onClick={handleCreateChannel} disabled={!newChName.trim()}>Add</button>
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

            {/* Existing roles */}
            {(activeServer?.roles ?? []).filter(r => r.name !== "@everyone").map(r => (
              <div key={r.id} className="organizer-row">
                <span className="organizer-name" style={{
                  color: typeof r.color === "number" && r.color > 0 ? `#${r.color.toString(16).padStart(6, '0')}` : undefined
                }}>
                  {r.name}
                </span>
                <div className="organizer-actions">
                  <button className="organizer-btn organizer-delete" onClick={async () => {
                    if (serverId) try { await api.deleteRole(serverId, r.id); } catch {}
                  }} title="Delete">x</button>
                </div>
              </div>
            ))}

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
        </div>
      </div>
    </div>
  );
}
