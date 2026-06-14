import { useState, useEffect, useRef, useCallback } from "react";
import { useApp, useActiveServer, useActiveServerId } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { toast } from "../lib/toast";
import type { ChannelInfo, CategoryInfo, MemberInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";

import InviteDialog from "./InviteDialog";
import ServerSettingsDialog from "./ServerSettingsDialog";
import ChannelSettingsDialog from "./ChannelSettingsDialog";
import UserProfilePopup from "./UserProfilePopup";
import NotificationSettings from "./NotificationSettings";
import SettingsModal from "./settings/SettingsModal";
import VoiceControlBar from "./VoiceControlBar";
import PeerVideoTiles from "./PeerVideoTiles";
import VoiceParticipantContextMenu from "./VoiceParticipantContextMenu";
import { useVoice } from "../hooks/useVoice";

function UserFooter({ members, roles }: { members: MemberInfo[]; roles: import("../lib/types").RoleInfo[] }) {
  const serverId = useActiveServerId();
  const [name, setName] = useState<string | null>(null);
  const [ownPk, setOwnPk] = useState<string | null>(null);
  const [profilePopup, setProfilePopup] = useState<{ x: number; y: number } | null>(null);
  const [showNotifSettings, setShowNotifSettings] = useState(false);
  const [showSettings, setShowSettings] = useState(false);

  useEffect(() => {
    api.getDisplayName().then((n) => setName(n)).catch(() => {});
    api.getPublicKey().then((pk) => setOwnPk(pk)).catch(() => {});
  }, []);

  const ownMember = ownPk ? members.find(m => publicKeyToString(m.public_key) === ownPk) ?? null : null;

  return (
    <>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", width: "100%" }}>
        <span
          style={{ cursor: ownMember ? "pointer" : undefined }}
          onClick={ownMember ? (e) => setProfilePopup({ x: e.clientX, y: e.clientY }) : undefined}
          title={ownMember ? "View your profile" : undefined}
        >
          ● {name ?? "Unknown"}
        </span>
        <button
          className="server-invite-btn"
          onClick={() => setShowSettings(true)}
          title="Settings"
          style={{ fontSize: 10, marginRight: 4 }}
        >⚙</button>
        <button
          className="server-invite-btn"
          onClick={() => setShowNotifSettings(true)}
          title="Notification Settings"
          style={{ fontSize: 10 }}
        >N</button>
      </div>
      {profilePopup && ownMember && serverId && (
        <UserProfilePopup
          member={ownMember}
          roles={roles}
          position={profilePopup}
          onClose={() => setProfilePopup(null)}
          isSelf={true}
          serverId={serverId}
        />
      )}
      {showNotifSettings && <NotificationSettings onClose={() => setShowNotifSettings(false)} />}
      {showSettings && <SettingsModal onClose={() => setShowSettings(false)} />}
    </>
  );
}

function CategoryEditForm({ category, onClose, serverId }: { category: CategoryInfo; onClose: () => void; serverId: string }) {
  const [name, setName] = useState(category.name);
  const [position, setPosition] = useState(String(category.position));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleSave() {
    setSaving(true);
    setError(null);
    try {
      const pos = parseInt(position, 10);
      await api.updateCategory(serverId, category.id, {
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
  const { dispatch } = useApp();
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const [showInvite, setShowInvite] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; channelId: number; type: "channel" | "category"; categoryId?: number } | null>(null);
  const [voiceMenu, setVoiceMenu] = useState<{ x: number; y: number; pubkeyHex: string; displayName: string } | null>(null);
  const [editChannel, setEditChannel] = useState<ChannelInfo | null>(null);
  const [editCategory, setEditCategory] = useState<CategoryInfo | null>(null);

  const voice = useVoice();

  // Self display-name initial, for the voice control bar avatar.
  const [selfName, setSelfName] = useState<string>("");
  useEffect(() => { void api.getDisplayName().then((n) => setSelfName(n ?? "")).catch(() => {}); }, []);
  // Own pubkey, to identify our own row in the voice participant list (our
  // speaking comes from voice.localSpeaking, not voice.peers which is others).
  const [ownPk, setOwnPk] = useState<string | null>(null);
  useEffect(() => { void api.getPublicKey().then(setOwnPk).catch(() => {}); }, []);

  // PTT mic mode + bound key, loaded from settings on mount. Read once here
  // (mode is applied at join; v1 does not hot-swap mid-call per the spec).
  const [micMode, setMicMode] = useState<string>("OpenMic");
  const [pttKey, setPttKey] = useState<string>("Backquote");
  useEffect(() => {
    api.getVoiceMode().then(setMicMode).catch(() => {});
    api.getPttKey().then(setPttKey).catch(() => {});
  }, []);

  // Window-level PTT keydown listener: active only while in a call AND mode is
  // PushToTalk. Ignores key repeats and typing in inputs/textareas/editables.
  const inCall = voice.inCall;
  const toggleTransmit = voice.toggleTransmit;
  useEffect(() => {
    if (!inCall || micMode !== "PushToTalk") return;
    const onKey = (e: KeyboardEvent) => {
      if (e.repeat) return;
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || t?.isContentEditable) return;
      if (e.code === pttKey) {
        e.preventDefault();
        void toggleTransmit();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [inCall, micMode, pttKey, toggleTransmit]);

  // Full voice-channel teardown: stop the audio engine AND clear presence
  // state. Shared by the channel-row toggle and the control-bar disconnect so
  // both paths leave the channel completely (not just stop audio).
  const leaveVoiceChannel = async (channelId: number) => {
    if (!serverId) return;
    await voice.leave().catch(() => {});
    await api.leaveVoice(serverId, channelId).catch(() => {});
    dispatch({ type: "LEAVE_VOICE_CHANNEL", serverId });
    // Re-sync the roster so we're removed from the participant list. A leaver
    // doesn't reliably receive their own MediaLeft broadcast, and
    // LEAVE_VOICE_CHANNEL only clears currentVoiceChannelId; without this the
    // server thinks we left but the list still shows us.
    try {
      const vs = await api.getVoiceState(serverId, channelId);
      const simplified = (vs || []).filter((v: any) => v && v.public_key).map((v: any) => ({
        publicKey: typeof v.public_key === "string" ? v.public_key : publicKeyToString(v.public_key),
        displayName: v.display_name || "Unknown",
      }));
      dispatch({ type: "SET_VOICE_STATE", serverId, payload: { channelId, participants: simplified } });
    } catch {}
  };

  const channels = activeServer?.channels ?? [];
  const categories = activeServer?.categories ?? [];
  const currentChannelId = activeServer?.currentChannelId ?? null;
  const readState = activeServer?.readState ?? {};
  const messages = activeServer?.messages ?? {};
  const dms = activeServer?.dms ?? [];
  const members = activeServer?.members ?? [];
  const roles = activeServer?.roles ?? [];

  // ── Drag state ────────────────────────────────────────────
  const dragRef = useRef<{ type: "channel" | "category"; id: number; startY: number } | null>(null);
  const [dragOverId, setDragOverId] = useState<number | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const isDraggingRef = useRef(false);
  const dragOverIdRef = useRef<number | null>(null);

  useEffect(() => { isDraggingRef.current = isDragging; }, [isDragging]);
  useEffect(() => { dragOverIdRef.current = dragOverId; }, [dragOverId]);

  const channelsRef = useRef(channels);
  channelsRef.current = channels;
  const categoriesRef = useRef(categories);
  categoriesRef.current = categories;

  // ── Swap helpers ──────────────────────────────────────────
  async function performChannelSwap(draggedId: number, targetId: number) {
    if (!serverId) return;
    const allChannels = channelsRef.current;
    const dragged = allChannels.find((c) => c.id === draggedId);
    const target = allChannels.find((c) => c.id === targetId);
    if (!dragged || !target) return;

    const targetIsCategory = categoriesRef.current.some((cat) => cat.id === targetId);
    if (targetIsCategory) {
      try { await api.updateChannel(serverId, draggedId, { categoryId: targetId, position: 0 }); } catch {}
      return;
    }

    if (dragged.category_id !== target.category_id) {
      try { await api.updateChannel(serverId, draggedId, { categoryId: target.category_id, position: target.position }); } catch {}
      return;
    }

    const siblings = allChannels
      .filter((c) => c.category_id === dragged.category_id)
      .sort((a, b) => a.position - b.position);

    try {
      for (let i = 0; i < siblings.length; i++) {
        if (siblings[i].position !== i) {
          await api.updateChannel(serverId, siblings[i].id, { position: i });
        }
      }
      const dragIdx = siblings.findIndex((c) => c.id === draggedId);
      const targetIdx = siblings.findIndex((c) => c.id === targetId);
      if (dragIdx !== -1 && targetIdx !== -1) {
        await api.updateChannel(serverId, draggedId, { position: targetIdx });
        await api.updateChannel(serverId, targetId, { position: dragIdx });
      }
    } catch {}
  }

  async function performCategorySwap(draggedId: number, targetId: number) {
    if (!serverId) return;
    const allCategories = categoriesRef.current;
    const sorted = [...allCategories].sort((a, b) => a.position - b.position);

    try {
      for (let i = 0; i < sorted.length; i++) {
        if (sorted[i].position !== i) {
          await api.updateCategory(serverId, sorted[i].id, { position: i });
        }
      }
      const dragIdx = sorted.findIndex((c) => c.id === draggedId);
      const targetIdx = sorted.findIndex((c) => c.id === targetId);
      if (dragIdx !== -1 && targetIdx !== -1) {
        await api.updateCategory(serverId, draggedId, { position: targetIdx });
        await api.updateCategory(serverId, sorted[targetIdx].id, { position: dragIdx });
      }
    } catch {}
  }

  // ── Global mousemove handler ───────────────────────────────
  const onMouseMove = useCallback((e: MouseEvent) => {
    if (!dragRef.current) return;
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

  // ── Global mouseup handler ─────────────────────────────────
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
      const targetIsCategory = categoriesRef.current.some((cat) => cat.id === overId);
      if (targetIsCategory) {
        performCategorySwap(drag.id, overId);
      }
    }
  }, [onMouseMove]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    return () => {
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };
  }, [onMouseMove, onMouseUp]);

  function startDrag(e: React.MouseEvent, type: "channel" | "category", id: number) {
    if (e.button !== 0) return;
    e.preventDefault();
    dragRef.current = { type, id, startY: e.clientY };
    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }

  async function handleSelectChannel(channel: ChannelInfo) {
    if (!serverId) return;
    dispatch({ type: "SELECT_CHANNEL", serverId, payload: channel.id });
    try {
      const msgs = await api.fetchHistory(serverId, channel.id);
      const reversed = msgs.reverse();
      dispatch({ type: "SET_MESSAGES", serverId, payload: { channelId: channel.id, messages: reversed } });
      if (reversed.length > 0) {
        const latestId = Math.max(...reversed.map((m) => m.id));
        dispatch({ type: "MARK_READ", serverId, payload: { channelId: channel.id, lastMessageId: latestId } });
      }
    } catch (e) {
      console.error("[channel] failed to load history:", e);
    }
  }

  async function handleMoveChannel(channelId: number, targetCategoryId: number | null, targetPosition: number) {
    if (!serverId) return;
    try { await api.updateChannel(serverId, channelId, { categoryId: targetCategoryId, position: targetPosition }); } catch {}
  }

  if (!activeServer || !serverId) {
    return <div className="channel-sidebar" />;
  }

  const visibleChannels = channels.filter((c) => c.channel_type !== "Thread" && c.channel_type !== "Dm");
  const sortedCategories = [...categories].sort((a, b) => a.position - b.position);
  const uncategorized = visibleChannels
    .filter((c) => c.category_id === null)
    .sort((a, b) => a.position - b.position);

  function renderVoiceChannel(ch: ChannelInfo) {
    const isInThisChannel = activeServer?.currentVoiceChannelId === ch.id;
    const participants = activeServer?.voiceStates[ch.id] ?? [];
    const liveByPk = new Map(voice.peers.map((p) => [p.pubkey, p]));
    return (
      <div key={ch.id}>
        <div
          data-drag-id={ch.id}
          data-drag-type="channel"
          className={`channel-item voice-channel${isInThisChannel ? " active" : ""}${dragOverId === ch.id ? " drag-over" : ""}`}
          onClick={async () => {
            if (!serverId) return;
            if (isInThisChannel) {
              await leaveVoiceChannel(ch.id);
            } else {
              // Register presence, then start the audio engine. Only commit the
              // "you're in this channel" UI state AFTER audio actually starts --
              // otherwise a silent audio failure leaves a zombie: marked in the
              // channel, on everyone's roster, but with no working control bar.
              try {
                await api.joinVoice(serverId, ch.id);
                await voice.join(serverId, ch.id);
              } catch (err) {
                console.error("[voice] join failed", err);
                // Roll back so we don't appear as a ghost on the server roster.
                await api.leaveVoice(serverId, ch.id).catch(() => {});
                await voice.leave().catch(() => {});
                toast.error(`Couldn't join voice channel: ${err}`);
                return;
              }
              dispatch({ type: "JOIN_VOICE_CHANNEL", serverId, payload: ch.id });
              try {
                const vs = await api.getVoiceState(serverId, ch.id);
                const simplified = (vs || []).filter((v: any) => v && v.public_key).map((v: any) => ({
                  publicKey: typeof v.public_key === "string" ? v.public_key : publicKeyToString(v.public_key),
                  displayName: v.display_name || "Unknown",
                }));
                dispatch({ type: "SET_VOICE_STATE", serverId, payload: { channelId: ch.id, participants: simplified } });
              } catch {}
            }
          }}
          onMouseDown={(e) => startDrag(e, "channel", ch.id)}
          onContextMenu={(e) => {
            e.preventDefault();
            setContextMenu({ x: e.clientX, y: e.clientY, channelId: ch.id, type: "channel" });
          }}
        >
          <span className="channel-prefix">~</span>
          <span>{ch.name}</span>
          {participants.length > 0 && (
            <span className="voice-count">({participants.length})</span>
          )}
        </div>
        {participants.length > 0 && (
          <div className="voice-participants">
            {participants.map((p) => {
              const live = liveByPk.get(p.publicKey);
              // Self speaks via localSpeaking; peers via their live peer state.
              const isSelf = ownPk != null && p.publicKey === ownPk;
              const isSpeaking = isSelf ? voice.localSpeaking : (live?.speaking ?? false);
              const initial = (p.displayName || "?").charAt(0).toUpperCase();
              return (
                <div
                  key={p.publicKey}
                  className={`voice-participant${isSpeaking ? " speaking" : ""}`}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setVoiceMenu({ x: e.clientX, y: e.clientY, pubkeyHex: p.publicKey, displayName: p.displayName });
                  }}
                >
                  <span className={`voice-avatar${isSpeaking ? " speaking" : ""}`}>{initial}</span>
                  <span className="voice-participant-name">{p.displayName}</span>
                  {live?.deafened
                    ? <span className="voice-participant-status" title="Deafened">&#x1F3A7;</span>
                    : live?.muted
                    ? <span className="voice-participant-status" title="Muted">&#x1F507;</span>
                    : null}
                  {voice.sharingPeers.has(p.publicKey) ? (
                    <button
                      className="voice-live-badge"
                      title="Watch screen share"
                      onClick={() => voice.toggleWatch(p.publicKey)}
                    >LIVE</button>
                  ) : null}
                </div>
              );
            })}
          </div>
        )}
      </div>
    );
  }

  function renderChannel(ch: ChannelInfo) {
    if (ch.channel_type === "Voice") return renderVoiceChannel(ch);
    const isActive = ch.id === currentChannelId;
    const lastRead = readState?.[ch.id] ?? 0;
    const channelMsgs = messages[ch.id] ?? [];
    const hasUnread = channelMsgs.some((m) => m.id > lastRead) && ch.id !== currentChannelId;
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
          <div className="server-name">{activeServer.serverName}</div>
          <div style={{ display: "flex", gap: "4px" }}>
            <button className="server-invite-btn" onClick={() => setShowSettings(true)} title="Server Settings">&#9881;</button>
            <button className="server-invite-btn" onClick={() => setShowInvite(true)} title="Create Invite">+</button>
          </div>
        </div>
        <div className="channel-list">
          {uncategorized.map(renderChannel)}
          {sortedCategories.map(renderCategory)}
          {/* Direct Messages */}
          {dms.length > 0 && (
            <>
              <div className="channel-category" style={{ marginTop: 8 }}>DIRECT MESSAGES</div>
              {dms.map(dm => {
                const isActive = currentChannelId === dm.channel.id;
                const lastRead = readState?.[dm.channel.id] ?? 0;
                const dmMsgs = messages[dm.channel.id] ?? [];
                const hasUnread = dmMsgs.some(m => m.id > lastRead) && !isActive;
                return (
                  <div
                    key={dm.channel.id}
                    className={`channel-item dm-item${isActive ? " active" : ""}${hasUnread ? " unread" : ""}`}
                    onClick={() => handleSelectChannel(dm.channel)}
                  >
                    <span className="dm-avatar-mini">{(dm.participant.display_name || "?").charAt(0).toUpperCase()}</span>
                    <span>{dm.participant.display_name}</span>
                  </div>
                );
              })}
            </>
          )}
        </div>
        {activeServer?.currentVoiceChannelId != null && <PeerVideoTiles />}
        {activeServer?.currentVoiceChannelId != null && (
          <VoiceControlBar
            voice={voice}
            channelName={channels.find((c) => c.id === activeServer.currentVoiceChannelId)?.name ?? "Voice"}
            selfInitial={(selfName || "Y").charAt(0).toUpperCase()}
            onDisconnect={() => leaveVoiceChannel(activeServer.currentVoiceChannelId!)}
          />
        )}
        <div className="user-footer">
          <UserFooter members={members} roles={roles} />
        </div>
      </div>
      {showInvite && <InviteDialog onClose={() => setShowInvite(false)} />}
      {showSettings && <ServerSettingsDialog onClose={() => setShowSettings(false)} />}
      {editChannel && <ChannelSettingsDialog channel={editChannel} onClose={() => setEditChannel(null)} />}
      {editCategory && serverId && (
        <div className="modal-overlay" onClick={() => setEditCategory(null)}>
          <div className="modal-dialog" onClick={(e) => e.stopPropagation()} style={{ minWidth: 300 }}>
            <div className="modal-titlebar">
              <span>Edit Category</span>
              <button className="modal-close" onClick={() => setEditCategory(null)}>X</button>
            </div>
            <div className="modal-body">
              <CategoryEditForm category={editCategory} onClose={() => setEditCategory(null)} serverId={serverId} />
            </div>
          </div>
        </div>
      )}
      {contextMenu && (
        <>
          <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={() => setContextMenu(null)} />
          <div className="context-menu" style={{ top: contextMenu.y, left: contextMenu.x }}>
            {contextMenu.type === "channel" && (() => {
              const ch = channels.find(c => c.id === contextMenu.channelId);
              const siblingsInCategory = visibleChannels
                .filter(c => c.category_id === (ch?.category_id ?? null))
                .sort((a, b) => a.position - b.position);
              const currentIndex = siblingsInCategory.findIndex(c => c.id === contextMenu.channelId);
              return (
                <>
                  <div className="context-menu-item" onClick={() => { if (ch) setEditChannel(ch); setContextMenu(null); }}>Edit Channel</div>
                  {currentIndex > 0 && (
                    <div className="context-menu-item" onClick={async () => {
                      if (!serverId) return;
                      try {
                        for (let i = 0; i < siblingsInCategory.length; i++) {
                          if (siblingsInCategory[i].position !== i) {
                            await api.updateChannel(serverId, siblingsInCategory[i].id, { position: i });
                          }
                        }
                        await api.updateChannel(serverId, contextMenu.channelId, { position: currentIndex - 1 });
                        await api.updateChannel(serverId, siblingsInCategory[currentIndex - 1].id, { position: currentIndex });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Up</div>
                  )}
                  {currentIndex < siblingsInCategory.length - 1 && (
                    <div className="context-menu-item" onClick={async () => {
                      if (!serverId) return;
                      try {
                        for (let i = 0; i < siblingsInCategory.length; i++) {
                          if (siblingsInCategory[i].position !== i) {
                            await api.updateChannel(serverId, siblingsInCategory[i].id, { position: i });
                          }
                        }
                        await api.updateChannel(serverId, contextMenu.channelId, { position: currentIndex + 1 });
                        await api.updateChannel(serverId, siblingsInCategory[currentIndex + 1].id, { position: currentIndex });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Down</div>
                  )}
                  {categories.length > 0 && (
                    <>
                      <div className="context-menu-separator" />
                      {categories
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
                    if (serverId) try { await api.deleteChannel(serverId, contextMenu.channelId); } catch {}
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
                    const cat = categories.find(c => c.id === contextMenu.categoryId);
                    if (cat) setEditCategory(cat);
                    setContextMenu(null);
                  }}>Edit Category</div>
                  {catIndex > 0 && (
                    <div className="context-menu-item" onClick={async () => {
                      if (!serverId) return;
                      try {
                        for (let i = 0; i < sortedCategories.length; i++) {
                          if (sortedCategories[i].position !== i) {
                            await api.updateCategory(serverId, sortedCategories[i].id, { position: i });
                          }
                        }
                        await api.updateCategory(serverId, contextMenu.categoryId!, { position: catIndex - 1 });
                        await api.updateCategory(serverId, sortedCategories[catIndex - 1].id, { position: catIndex });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Up</div>
                  )}
                  {catIndex < sortedCategories.length - 1 && (
                    <div className="context-menu-item" onClick={async () => {
                      if (!serverId) return;
                      try {
                        for (let i = 0; i < sortedCategories.length; i++) {
                          if (sortedCategories[i].position !== i) {
                            await api.updateCategory(serverId, sortedCategories[i].id, { position: i });
                          }
                        }
                        await api.updateCategory(serverId, contextMenu.categoryId!, { position: catIndex + 1 });
                        await api.updateCategory(serverId, sortedCategories[catIndex + 1].id, { position: catIndex });
                      } catch {}
                      setContextMenu(null);
                    }}>Move Down</div>
                  )}
                  <div className="context-menu-separator" />
                  <div className="context-menu-item delete" onClick={async () => {
                    if (serverId) try { await api.deleteCategory(serverId, contextMenu.categoryId!); } catch {}
                    setContextMenu(null);
                  }}>Delete Category</div>
                </>
              );
            })()}
          </div>
        </>
      )}
      {voiceMenu && (
        <VoiceParticipantContextMenu
          pubkeyHex={voiceMenu.pubkeyHex}
          displayName={voiceMenu.displayName}
          position={{ x: voiceMenu.x, y: voiceMenu.y }}
          currentVolume={voice.peerVolume(voiceMenu.pubkeyHex)}
          onSetVolume={(v) => { void voice.setPeerVolume(voiceMenu.pubkeyHex, v); }}
          onClose={() => setVoiceMenu(null)}
        />
      )}
    </>
  );
}
