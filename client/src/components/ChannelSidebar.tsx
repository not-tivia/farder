import { useState, useEffect, useRef, useCallback } from "react";
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

  // ── Drag state ────────────────────────────────────────────
  const dragRef = useRef<{ type: "channel" | "category"; id: number; startY: number } | null>(null);
  const [dragOverId, setDragOverId] = useState<number | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const isDraggingRef = useRef(false);
  const dragOverIdRef = useRef<number | null>(null);

  // Keep refs in sync with state so stable callbacks read current values
  useEffect(() => { isDraggingRef.current = isDragging; }, [isDragging]);
  useEffect(() => { dragOverIdRef.current = dragOverId; }, [dragOverId]);

  // Stable refs for latest channels/categories (avoids stale closures in callbacks)
  const channelsRef = useRef(state.channels);
  channelsRef.current = state.channels;
  const categoriesRef = useRef(state.categories);
  categoriesRef.current = state.categories;

  // ── Swap helpers ──────────────────────────────────────────
  async function performChannelSwap(draggedId: number, targetId: number) {
    const allChannels = channelsRef.current;
    const dragged = allChannels.find((c) => c.id === draggedId);
    const target = allChannels.find((c) => c.id === targetId);
    if (!dragged || !target) return;

    // Dragged over a category header → move channel into that category
    const targetIsCategory = categoriesRef.current.some((cat) => cat.id === targetId);
    if (targetIsCategory) {
      try {
        await api.updateChannel(draggedId, { categoryId: targetId, position: 0 });
      } catch {}
      return;
    }

    // Different categories → move dragged channel to target's category at target's position
    if (dragged.category_id !== target.category_id) {
      try {
        await api.updateChannel(draggedId, { categoryId: target.category_id, position: target.position });
      } catch {}
      return;
    }

    // Same category → swap positions
    const siblings = allChannels
      .filter((c) => c.category_id === dragged.category_id)
      .sort((a, b) => a.position - b.position);

    try {
      // Normalize positions first
      for (let i = 0; i < siblings.length; i++) {
        if (siblings[i].position !== i) {
          await api.updateChannel(siblings[i].id, { position: i });
        }
      }
      const dragIdx = siblings.findIndex((c) => c.id === draggedId);
      const targetIdx = siblings.findIndex((c) => c.id === targetId);
      if (dragIdx !== -1 && targetIdx !== -1) {
        await api.updateChannel(draggedId, { position: targetIdx });
        await api.updateChannel(targetId, { position: dragIdx });
      }
    } catch {}
  }

  async function performCategorySwap(draggedId: number, targetId: number) {
    const allCategories = categoriesRef.current;
    const sorted = [...allCategories].sort((a, b) => a.position - b.position);

    try {
      // Normalize positions first
      for (let i = 0; i < sorted.length; i++) {
        if (sorted[i].position !== i) {
          await api.updateCategory(sorted[i].id, { position: i });
        }
      }
      const dragIdx = sorted.findIndex((c) => c.id === draggedId);
      const targetIdx = sorted.findIndex((c) => c.id === targetId);
      if (dragIdx !== -1 && targetIdx !== -1) {
        await api.updateCategory(draggedId, { position: targetIdx });
        await api.updateCategory(sorted[targetIdx].id, { position: dragIdx });
      }
    } catch {}
  }

  // ── Global mousemove handler (stable reference) ───────────
  const onMouseMove = useCallback((e: MouseEvent) => {
    if (!dragRef.current) return;

    // Require 5px movement before starting visual drag
    if (!isDraggingRef.current && Math.abs(e.clientY - dragRef.current.startY) < 5) return;
    if (!isDraggingRef.current) setIsDragging(true);

    const elements = document.querySelectorAll("[data-drag-id]");
    let hoveredId: number | null = null;
    for (const el of elements) {
      const rect = (el as HTMLElement).getBoundingClientRect();
      if (e.clientY >= rect.top && e.clientY <= rect.bottom) {
        hoveredId = Number(el.getAttribute("data-drag-id"));
        break;
      }
    }
    if (hoveredId !== dragOverIdRef.current) setDragOverId(hoveredId);
  }, []);

  // ── Global mouseup handler (stable reference) ─────────────
  const onMouseUp = useCallback(() => {
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);

    const drag = dragRef.current;
    const overId = dragOverIdRef.current;
    const wasDragging = isDraggingRef.current;

    dragRef.current = null;
    setIsDragging(false);
    setDragOverId(null);

    if (!drag || !wasDragging || overId === null || overId === drag.id) return;

    if (drag.type === "channel") {
      performChannelSwap(drag.id, overId);
    } else {
      // Category dragged over another category header
      const targetIsCategory = categoriesRef.current.some((cat) => cat.id === overId);
      if (targetIsCategory) {
        performCategorySwap(drag.id, overId);
      }
    }
  }, [onMouseMove]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Cleanup on unmount ────────────────────────────────────
  useEffect(() => {
    return () => {
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };
  }, [onMouseMove, onMouseUp]);

  // ── Start drag ────────────────────────────────────────────
  function startDrag(e: React.MouseEvent, type: "channel" | "category", id: number) {
    if (e.button !== 0) return;
    e.preventDefault();
    dragRef.current = { type, id, startY: e.clientY };
    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }

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
        data-drag-id={ch.id}
        data-drag-type="channel"
        className={`channel-item${isActive ? " active" : ""}${hasUnread ? " unread" : ""}${dragOverId === ch.id ? " drag-over" : ""}`}
        onClick={() => handleSelectChannel(ch)}
        onMouseDown={(e) => startDrag(e, "channel", ch.id)}
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
          data-drag-id={cat.id}
          data-drag-type="category"
          className={`channel-category${dragOverId === cat.id ? " drag-over" : ""}`}
          onMouseDown={(e) => startDrag(e, "category", cat.id)}
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
      <div className={`channel-sidebar${isDragging ? " dragging" : ""}`}>
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
                      // Re-assign sequential positions, then swap the two
                      try {
                        for (let i = 0; i < siblingsInCategory.length; i++) {
                          if (siblingsInCategory[i].position !== i) {
                            await api.updateChannel(siblingsInCategory[i].id, { position: i });
                          }
                        }
                        // Now swap current with above
                        await api.updateChannel(contextMenu.channelId, { position: currentIndex - 1 });
                        await api.updateChannel(siblingsInCategory[currentIndex - 1].id, { position: currentIndex });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Up</div>
                  )}
                  {currentIndex < siblingsInCategory.length - 1 && (
                    <div className="context-menu-item" onClick={async () => {
                      try {
                        for (let i = 0; i < siblingsInCategory.length; i++) {
                          if (siblingsInCategory[i].position !== i) {
                            await api.updateChannel(siblingsInCategory[i].id, { position: i });
                          }
                        }
                        await api.updateChannel(contextMenu.channelId, { position: currentIndex + 1 });
                        await api.updateChannel(siblingsInCategory[currentIndex + 1].id, { position: currentIndex });
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
                      try {
                        for (let i = 0; i < sortedCategories.length; i++) {
                          if (sortedCategories[i].position !== i) {
                            await api.updateCategory(sortedCategories[i].id, { position: i });
                          }
                        }
                        await api.updateCategory(contextMenu.categoryId!, { position: catIndex - 1 });
                        await api.updateCategory(sortedCategories[catIndex - 1].id, { position: catIndex });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Up</div>
                  )}
                  {catIndex < sortedCategories.length - 1 && (
                    <div className="context-menu-item" onClick={async () => {
                      try {
                        for (let i = 0; i < sortedCategories.length; i++) {
                          if (sortedCategories[i].position !== i) {
                            await api.updateCategory(sortedCategories[i].id, { position: i });
                          }
                        }
                        await api.updateCategory(contextMenu.categoryId!, { position: catIndex + 1 });
                        await api.updateCategory(sortedCategories[catIndex + 1].id, { position: catIndex });
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
