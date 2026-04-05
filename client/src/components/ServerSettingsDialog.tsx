import { useState } from "react";
import * as api from "../lib/tauri-bridge";
import { useServer } from "../context/ServerContext";
import type { ChannelInfo } from "../lib/types";

interface Props { onClose: () => void; }

export default function ServerSettingsDialog({ onClose }: Props) {
  const { state } = useServer();
  const [newChName, setNewChName] = useState("");
  const [newChType, setNewChType] = useState("Text");
  const [newChCatId, setNewChCatId] = useState<number | undefined>(undefined);
  const [newCatName, setNewCatName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const sortedCategories = [...state.categories].sort((a, b) => a.position - b.position);
  const allChannels = state.channels.filter(c => c.channel_type !== "Thread");
  const uncategorized = allChannels.filter(c => c.category_id === null).sort((a, b) => a.position - b.position);

  function channelsInCategory(catId: number) {
    return allChannels.filter(c => c.category_id === catId).sort((a, b) => a.position - b.position);
  }

  // Normalize and swap positions for channels within a group
  async function swapChannels(channels: ChannelInfo[], index: number, direction: -1 | 1) {
    const target = index + direction;
    if (target < 0 || target >= channels.length) return;
    try {
      // Normalize first
      for (let i = 0; i < channels.length; i++) {
        if (channels[i].position !== i) {
          await api.updateChannel(channels[i].id, { position: i });
        }
      }
      await api.updateChannel(channels[index].id, { position: target });
      await api.updateChannel(channels[target].id, { position: index });
    } catch (e) { setError(String(e)); }
  }

  async function swapCategories(index: number, direction: -1 | 1) {
    const target = index + direction;
    if (target < 0 || target >= sortedCategories.length) return;
    try {
      for (let i = 0; i < sortedCategories.length; i++) {
        if (sortedCategories[i].position !== i) {
          await api.updateCategory(sortedCategories[i].id, { position: i });
        }
      }
      await api.updateCategory(sortedCategories[index].id, { position: target });
      await api.updateCategory(sortedCategories[target].id, { position: index });
    } catch (e) { setError(String(e)); }
  }

  async function moveChannelToCategory(channelId: number, categoryId: number | null) {
    try {
      await api.updateChannel(channelId, { categoryId, position: 0 });
    } catch (e) { setError(String(e)); }
  }

  async function handleCreateChannel() {
    if (!newChName.trim()) return;
    try {
      await api.createChannel(newChName.trim(), newChType, newChCatId);
      setNewChName("");
    } catch (e) { setError(String(e)); }
  }

  async function handleCreateCategory() {
    if (!newCatName.trim()) return;
    try {
      await api.createCategory(newCatName.trim());
      setNewCatName("");
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
            try { await api.deleteChannel(ch.id); } catch {}
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
            {/* Uncategorized channels */}
            {uncategorized.length > 0 && (
              <div className="organizer-group">
                <div className="organizer-group-header">Uncategorized</div>
                {uncategorized.map((ch, i) => renderChannelRow(ch, uncategorized, i))}
              </div>
            )}

            {/* Categories with their channels */}
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
                        try { await api.deleteCategory(cat.id); } catch {}
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

          {/* Create new */}
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
        </div>
      </div>
    </div>
  );
}
