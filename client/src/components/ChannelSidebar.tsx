import { useState, useEffect } from "react";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import type { ChannelInfo, CategoryInfo } from "../lib/types";
import InviteDialog from "./InviteDialog";
import ServerSettingsDialog from "./ServerSettingsDialog";
import ChannelSettingsDialog from "./ChannelSettingsDialog";

function UserFooter() {
  const [name, setName] = useState<string | null>(null);

  useEffect(() => {
    api.getDisplayName().then((n) => setName(n)).catch(() => {});
  }, []);

  return <span>● {name ?? "Unknown"}</span>;
}

function CategoryEditForm({ category, onClose }: { category: CategoryInfo; onClose: () => void }) {
  const [name, setName] = useState(category.name);
  const [position, setPosition] = useState(String(category.position));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleSave() {
    setSaving(true);
    setError(null);
    try {
      const pos = parseInt(position, 10);
      await api.updateCategory(category.id, {
        name: name !== category.name ? name : undefined,
        position: !isNaN(pos) && pos !== category.position ? pos : undefined,
      });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div>
      <div className="connect-section">
        <label className="connect-label">Category Name</label>
        <input className="connect-input" value={name} onChange={(e) => setName(e.target.value)} />
      </div>
      <div className="connect-section">
        <label className="connect-label">Position</label>
        <input className="connect-input" type="number" min="0" value={position} onChange={(e) => setPosition(e.target.value)} />
      </div>
      {error && <div className="error-text">{error}</div>}
      <div className="connect-actions">
        <button className="xp-button" onClick={handleSave} disabled={saving}>
          {saving ? "Saving..." : "Save Changes"}
        </button>
      </div>
    </div>
  );
}

export default function ChannelSidebar() {
  const { state, dispatch } = useServer();
  const [showInvite, setShowInvite] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; channelId: number; type: "channel" | "category"; categoryId?: number } | null>(null);
  const [editChannel, setEditChannel] = useState<ChannelInfo | null>(null);
  const [editCategory, setEditCategory] = useState<CategoryInfo | null>(null);

  async function handleSelectChannel(channel: ChannelInfo) {
    dispatch({ type: "SELECT_CHANNEL", payload: channel.id });
    try {
      await api.subscribeChannels([channel.id]);
      const msgs = await api.fetchHistory(channel.id);
      // Server returns newest-first; reverse for chronological display
      const reversed = msgs.reverse();
      dispatch({ type: "SET_MESSAGES", payload: { channelId: channel.id, messages: reversed } });
      // Mark channel as read with latest message id
      if (reversed.length > 0) {
        const latestId = Math.max(...reversed.map((m) => m.id));
        dispatch({ type: "MARK_READ", payload: { channelId: channel.id, lastMessageId: latestId } });
      }
    } catch {
      // ignore fetch errors
    }
  }

  async function handleMoveChannel(channelId: number, targetCategoryId: number | null, targetPosition: number) {
    try {
      await api.updateChannel(channelId, { categoryId: targetCategoryId, position: targetPosition });
    } catch {}
  }


  // Exclude Thread channels from the sidebar (they appear inline)
  const visibleChannels = state.channels.filter((c) => c.channel_type !== "Thread");
  const sortedCategories = [...state.categories].sort((a, b) => a.position - b.position);
  const uncategorized = visibleChannels
    .filter((c) => c.category_id === null)
    .sort((a, b) => a.position - b.position);

  function renderChannel(ch: ChannelInfo) {
    const isActive = ch.id === state.currentChannelId;
    const lastRead = state.readState?.[ch.id] ?? 0;
    const channelMsgs = state.messages[ch.id] ?? [];
    const hasUnread = channelMsgs.some((m) => m.id > lastRead) && ch.id !== state.currentChannelId;
    const prefix = ch.channel_type === "Announcement" ? "!" : "#";
    return (
      <div
        key={ch.id}
        className={`channel-item${isActive ? " active" : ""}${hasUnread ? " unread" : ""}`}
        onClick={() => handleSelectChannel(ch)}
        onContextMenu={(e) => {
          e.preventDefault();
          setContextMenu({ x: e.clientX, y: e.clientY, channelId: ch.id, type: "channel" });
        }}
      >
        <span className="channel-prefix">{prefix}</span>
        <span>{ch.name}</span>
      </div>
    );
  }

  function renderCategory(cat: CategoryInfo) {
    const catChannels = visibleChannels
      .filter((c) => c.category_id === cat.id)
      .sort((a, b) => a.position - b.position);
    if (catChannels.length === 0) return null;
    return (
      <div key={cat.id}>
        <div
          className="channel-category"
          onContextMenu={(e) => {
            e.preventDefault();
            setContextMenu({ x: e.clientX, y: e.clientY, channelId: 0, type: "category", categoryId: cat.id });
          }}
        >{cat.name}</div>
        {catChannels.map(renderChannel)}
      </div>
    );
  }

  return (
    <>
      <div className="channel-sidebar">
        <div className="server-header">
          <div className="server-name">{state.serverName}</div>
          <div style={{ display: "flex", gap: "4px" }}>
            <button className="server-invite-btn" onClick={() => setShowSettings(true)} title="Server Settings">&#9881;</button>
            <button className="server-invite-btn" onClick={() => setShowInvite(true)} title="Create Invite">+</button>
          </div>
        </div>
        <div className="channel-list">
          {uncategorized.map(renderChannel)}
          {sortedCategories.map(renderCategory)}
        </div>
        <div className="user-footer">
          <UserFooter />
        </div>
      </div>
      {showInvite && <InviteDialog onClose={() => setShowInvite(false)} />}
      {showSettings && <ServerSettingsDialog onClose={() => setShowSettings(false)} />}
      {editChannel && <ChannelSettingsDialog channel={editChannel} onClose={() => setEditChannel(null)} />}
      {editCategory && (
        <div className="modal-overlay" onClick={() => setEditCategory(null)}>
          <div className="modal-dialog" onClick={(e) => e.stopPropagation()} style={{ minWidth: 300 }}>
            <div className="modal-titlebar">
              <span>Edit Category</span>
              <button className="modal-close" onClick={() => setEditCategory(null)}>X</button>
            </div>
            <div className="modal-body">
              <CategoryEditForm category={editCategory} onClose={() => setEditCategory(null)} />
            </div>
          </div>
        </div>
      )}
      {contextMenu && (
        <>
          <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={() => setContextMenu(null)} />
          <div className="context-menu" style={{ top: contextMenu.y, left: contextMenu.x }}>
            {contextMenu.type === "channel" && (() => {
              const ch = state.channels.find(c => c.id === contextMenu.channelId);
              const siblingsInCategory = visibleChannels
                .filter(c => c.category_id === (ch?.category_id ?? null))
                .sort((a, b) => a.position - b.position);
              const currentIndex = siblingsInCategory.findIndex(c => c.id === contextMenu.channelId);
              return (
                <>
                  <div className="context-menu-item" onClick={() => { if (ch) setEditChannel(ch); setContextMenu(null); }}>Edit Channel</div>
                  {currentIndex > 0 && (
                    <div className="context-menu-item" onClick={async () => {
                      const above = siblingsInCategory[currentIndex - 1];
                      const myPos = ch!.position;
                      const theirPos = above.position;
                      // Swap positions
                      try {
                        await api.updateChannel(contextMenu.channelId, { position: theirPos });
                        await api.updateChannel(above.id, { position: myPos });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Up</div>
                  )}
                  {currentIndex < siblingsInCategory.length - 1 && (
                    <div className="context-menu-item" onClick={async () => {
                      const below = siblingsInCategory[currentIndex + 1];
                      const myPos = ch!.position;
                      const theirPos = below.position;
                      // Swap positions
                      try {
                        await api.updateChannel(contextMenu.channelId, { position: theirPos });
                        await api.updateChannel(below.id, { position: myPos });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Down</div>
                  )}
                  {state.categories.length > 0 && (
                    <>
                      <div className="context-menu-separator" />
                      {state.categories
                        .filter(cat => cat.id !== ch?.category_id)
                        .map(cat => (
                          <div key={cat.id} className="context-menu-item" onClick={async () => {
                            await handleMoveChannel(contextMenu.channelId, cat.id, 0);
                            setContextMenu(null);
                          }}>Move to {cat.name}</div>
                        ))
                      }
                      {ch?.category_id !== null && (
                        <div className="context-menu-item" onClick={async () => {
                          await handleMoveChannel(contextMenu.channelId, null, 0);
                          setContextMenu(null);
                        }}>Remove from Category</div>
                      )}
                    </>
                  )}
                  <div className="context-menu-separator" />
                  <div className="context-menu-item delete" onClick={async () => {
                    try { await api.deleteChannel(contextMenu.channelId); } catch {}
                    setContextMenu(null);
                  }}>Delete Channel</div>
                </>
              );
            })()}
            {contextMenu.type === "category" && (() => {
              const catIndex = sortedCategories.findIndex(c => c.id === contextMenu.categoryId);
              return (
                <>
                  <div className="context-menu-item" onClick={() => {
                    const cat = state.categories.find(c => c.id === contextMenu.categoryId);
                    if (cat) setEditCategory(cat);
                    setContextMenu(null);
                  }}>Edit Category</div>
                  {catIndex > 0 && (
                    <div className="context-menu-item" onClick={async () => {
                      const myCat = sortedCategories[catIndex];
                      const above = sortedCategories[catIndex - 1];
                      try {
                        await api.updateCategory(contextMenu.categoryId!, { position: above.position });
                        await api.updateCategory(above.id, { position: myCat.position });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Up</div>
                  )}
                  {catIndex < sortedCategories.length - 1 && (
                    <div className="context-menu-item" onClick={async () => {
                      const myCat = sortedCategories[catIndex];
                      const below = sortedCategories[catIndex + 1];
                      try {
                        await api.updateCategory(contextMenu.categoryId!, { position: below.position });
                        await api.updateCategory(below.id, { position: myCat.position });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Down</div>
                  )}
                  <div className="context-menu-separator" />
                  <div className="context-menu-item delete" onClick={async () => {
                    try { await api.deleteCategory(contextMenu.categoryId!); } catch {}
                    setContextMenu(null);
                  }}>Delete Category</div>
                </>
              );
            })()}
          </div>
        </>
      )}
    </>
  );
}
