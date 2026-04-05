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
  const [dragItem, setDragItem] = useState<{ type: "channel" | "category"; id: number } | null>(null);
  const [dropTarget, setDropTarget] = useState<{ type: "channel" | "category" | "category-zone"; id: number } | null>(null);

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
      console.log("[drag] moveChannel", channelId, "to cat", targetCategoryId, "pos", targetPosition);
      await api.updateChannel(channelId, { categoryId: targetCategoryId, position: targetPosition });
      console.log("[drag] moveChannel success");
    } catch (e) {
      console.error("[drag] moveChannel failed:", e);
    }
  }

  async function handleMoveCategory(categoryId: number, targetPosition: number) {
    try {
      console.log("[drag] moveCategory", categoryId, "to pos", targetPosition);
      await api.updateCategory(categoryId, { position: targetPosition });
      console.log("[drag] moveCategory success");
    } catch (e) {
      console.error("[drag] moveCategory failed:", e);
    }
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
    const isDropTarget = dropTarget?.type === "channel" && dropTarget.id === ch.id;
    return (
      <div
        key={ch.id}
        className={`channel-item${isActive ? " active" : ""}${hasUnread ? " unread" : ""}${isDropTarget ? " drop-target" : ""}`}
        onClick={() => handleSelectChannel(ch)}
        draggable
        onDragStart={(e) => {
          setDragItem({ type: "channel", id: ch.id });
          e.dataTransfer.effectAllowed = "move";
        }}
        onDragOver={(e) => {
          e.preventDefault();
          if (dragItem?.type === "channel") {
            setDropTarget({ type: "channel", id: ch.id });
          }
        }}
        onDragLeave={() => {
          if (dropTarget?.id === ch.id) setDropTarget(null);
        }}
        onDrop={async (e) => {
          e.preventDefault();
          if (dragItem?.type === "channel" && dragItem.id !== ch.id) {
            await handleMoveChannel(dragItem.id, ch.category_id, ch.position);
          }
          setDragItem(null);
          setDropTarget(null);
        }}
        onDragEnd={() => { setDragItem(null); setDropTarget(null); }}
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
    const isCategoryDropTarget = dropTarget?.type === "category" && dropTarget.id === cat.id;
    const isCategoryZoneTarget = dropTarget?.type === "category-zone" && dropTarget.id === cat.id;
    return (
      <div key={cat.id}>
        <div
          className={`channel-category${isCategoryDropTarget || isCategoryZoneTarget ? " drop-target" : ""}`}
          draggable
          onDragStart={(e) => {
            setDragItem({ type: "category", id: cat.id });
            e.dataTransfer.effectAllowed = "move";
          }}
          onDragOver={(e) => {
            e.preventDefault();
            if (dragItem?.type === "channel") {
              setDropTarget({ type: "category-zone", id: cat.id });
            } else if (dragItem?.type === "category") {
              setDropTarget({ type: "category", id: cat.id });
            }
          }}
          onDragLeave={() => {
            if (dropTarget?.id === cat.id) setDropTarget(null);
          }}
          onDrop={async (e) => {
            e.preventDefault();
            if (dragItem?.type === "channel") {
              await handleMoveChannel(dragItem.id, cat.id, 0);
            } else if (dragItem?.type === "category" && dragItem.id !== cat.id) {
              await handleMoveCategory(dragItem.id, cat.position);
            }
            setDragItem(null);
            setDropTarget(null);
          }}
          onDragEnd={() => { setDragItem(null); setDropTarget(null); }}
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
            {contextMenu.type === "channel" && (
              <>
                <div className="context-menu-item" onClick={() => {
                  const ch = state.channels.find(c => c.id === contextMenu.channelId);
                  if (ch) setEditChannel(ch);
                  setContextMenu(null);
                }}>Edit Channel</div>
                <div className="context-menu-item delete" onClick={async () => {
                  try { await api.deleteChannel(contextMenu.channelId); } catch {}
                  setContextMenu(null);
                }}>Delete Channel</div>
              </>
            )}
            {contextMenu.type === "category" && (
              <>
                <div className="context-menu-item" onClick={() => {
                  const cat = state.categories.find(c => c.id === contextMenu.categoryId);
                  if (cat) setEditCategory(cat);
                  setContextMenu(null);
                }}>Edit Category</div>
                <div className="context-menu-item delete" onClick={async () => {
                  try { await api.deleteCategory(contextMenu.categoryId!); } catch {}
                  setContextMenu(null);
                }}>Delete Category</div>
              </>
            )}
          </div>
        </>
      )}
    </>
  );
}
