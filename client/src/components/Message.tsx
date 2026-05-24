import { useState, useEffect } from "react";
import type { MessageInfo, AttachmentInfo } from "../lib/types";
import { publicKeyToString, isDeletedUser } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { useApp, useActiveServer } from "../context/ServerContext";
import ReactionPicker from "./ReactionPicker";
import UserProfilePopup from "./UserProfilePopup";
import MemberContextMenu from "./MemberContextMenu";
import RenderedMessageContent from "./RenderedMessageContent";
import TimedOutBadge from "./TimedOutBadge";
import { getActorPermissions, isModerator } from "../lib/permissions";
import { renderUnicodeEmoji } from "../lib/unicodeEmoji";
import { TranslatedRow } from "./TranslatedRow";
import { TranslationDownloadDialog } from "./TranslationDownloadDialog";
import { translateMessage, subscribe as subscribeTranslation, dismiss as dismissTranslation } from "../lib/translation/store";
import { getTranslationSettings } from "../lib/translation/api";

interface MessageProps {
  message: MessageInfo;
  memberNames: Record<string, string>;
  grouped?: boolean;
  serverId: string;
  highlighted?: boolean;
  onReply?: (message: MessageInfo) => void;
}

/** Derive a deterministic color from a string (public key or name). */
function authorColor(key: string): string {
  let hash = 0;
  for (let i = 0; i < key.length; i++) {
    hash = (hash * 31 + key.charCodeAt(i)) >>> 0;
  }
  const hue = hash % 360;
  return `hsl(${hue}, 65%, 38%)`;
}

function formatTimestamp(ts: number): string {
  try {
    const date = new Date(ts * 1000);
    const now = new Date();
    const isToday = date.toDateString() === now.toDateString();
    const yesterday = new Date(now);
    yesterday.setDate(yesterday.getDate() - 1);
    const isYesterday = date.toDateString() === yesterday.toDateString();

    const time = date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

    if (isToday) return `Today at ${time}`;
    if (isYesterday) return `Yesterday at ${time}`;
    return `${date.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" })} at ${time}`;
  } catch {
    return String(ts);
  }
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

// Module-level cache: file_id → data URL
const imageCache = new Map<number, string>();

// Module-level cache for own public key
let cachedOwnPk: string | null = null;

// Module-level cache for own display name
let cachedOwnDisplayName: string | null = null;

// Module-level cache for book index
let cachedBookIndex: BookItem[] = [];
let bookIndexLoadPromise: Promise<void> | null = null;
function loadBookIndex(): Promise<void> {
  if (!bookIndexLoadPromise) {
    bookIndexLoadPromise = bookApi.bookListItems()
      .then((items) => { cachedBookIndex = items; })
      .catch(() => {});
  }
  return bookIndexLoadPromise;
}

function renderContent(text: string, memberNames: Record<string, string>, ownDisplayName: string | null) {
  const parts = text.split(/(@\w+)/g);
  return parts.map((part, i) => {
    if (part.startsWith("@")) {
      const name = part.slice(1);
      const isMention =
        name === "everyone" ||
        Object.values(memberNames).some((n) => n.toLowerCase() === name.toLowerCase());
      const isSelfMention =
        ownDisplayName != null && name.toLowerCase() === ownDisplayName.toLowerCase();
      if (isMention) {
        return (
          <span key={i} className={`mention${isSelfMention ? " mention-self" : ""}`}>
            {part}
          </span>
        );
      }
    }
    return <span key={i}>{part}</span>;
  });
}

export default function Message({ message, memberNames, grouped = false, serverId, highlighted = false, onReply }: MessageProps) {
  const { dispatch } = useApp();
  const activeServer = useActiveServer();
  const [showPicker, setShowPicker] = useState(false);
  const [reacting, setReacting] = useState(false);
  const [profilePopup, setProfilePopup] = useState<{ x: number; y: number } | null>(null);
  const [ownPk, setOwnPk] = useState(cachedOwnPk);
  const [ownDisplayName, setOwnDisplayName] = useState(cachedOwnDisplayName);
  const [bookIndex, setBookIndex] = useState<BookItem[]>(cachedBookIndex);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [memberMenu, setMemberMenu] = useState<{ x: number; y: number } | null>(null);
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState("");

  const [pendingDownload, setPendingDownload] = useState<{
    pair: { src: string; trg: string };
    resolve: () => void;
    reject: (reason: unknown) => void;
    inProgress: boolean;
  } | null>(null);

  const [translationSettings, setTranslationSettings] = useState<{
    enabled: boolean;
    default_target: string;
  } | null>(null);

  useEffect(() => {
    if (!cachedOwnPk) {
      api.getPublicKey().then((pk) => { cachedOwnPk = pk; setOwnPk(pk); });
    }
    if (!cachedOwnDisplayName) {
      api.getDisplayName().then((name) => { cachedOwnDisplayName = name; setOwnDisplayName(name); });
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    loadBookIndex().then(() => {
      if (!cancelled) setBookIndex(cachedBookIndex);
    });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    getTranslationSettings().then((s) =>
      setTranslationSettings({ enabled: s.enabled, default_target: s.default_target })
    );
  }, []);

  const deleted = isDeletedUser(message.author);
  const pkStr = publicKeyToString(message.author);
  const displayName = deleted
    ? "Deleted User"
    : (memberNames[pkStr] ?? pkStr.slice(0, 16) + "…");
  const color = deleted ? "#999" : authorColor(pkStr);
  const member = deleted ? null : (activeServer?.members.find((m) => publicKeyToString(m.public_key) === pkStr) ?? null);
  const roles = activeServer?.roles ?? [];

  const isOwnMessage = ownPk === pkStr;

  const { bits: viewerBits } = ownPk
    ? getActorPermissions(activeServer?.members ?? [], roles, ownPk, activeServer?.ownerPublicKey ?? null)
    : { bits: 0n };
  const showModBadges = isModerator(viewerBits);

  // Strip image URLs from message text when there are image attachments
  const displayContent = deleted
    ? message.content
    : message.attachments.length > 0
      ? message.content.replace(/https?:\/\/[^\s]+\.(?:png|jpg|jpeg|gif|webp)(?:\?[^\s]*)?/gi, "").trim()
      : message.content;

  async function handleReactionClick(emoji: string, alreadyMe: boolean, fileId?: number) {
    if (reacting) return;
    setReacting(true);
    try {
      if (alreadyMe) {
        await api.removeReaction(serverId, message.id, emoji, fileId);
      } else {
        await api.addReaction(serverId, message.id, emoji, fileId);
      }
    } catch {
      // ignore
    } finally {
      setReacting(false);
    }
  }

  async function handlePickerSelect(emoji: string) {
    if (reacting) return;
    setShowPicker(false);
    setReacting(true);
    try {
      await api.addReaction(serverId, message.id, emoji);
    } catch {
      // ignore
    } finally {
      setReacting(false);
    }
  }

  async function handlePickerBookSelect(item: BookItem) {
    if (reacting) return;
    setShowPicker(false);
    setReacting(true);
    try {
      const fileId = await bookApi.bookGetFileForServer(serverId, item.id);
      await api.addReaction(serverId, message.id, ":custom:", fileId);
    } catch (e) {
      console.error("[reaction:book] failed:", e);
    } finally {
      setReacting(false);
    }
  }

  function handleThreadClick() {
    if (message.thread_id !== null) {
      dispatch({ type: "VIEW_THREAD", serverId, payload: message.thread_id });
    }
  }

  async function handleSaveEdit() {
    if (!editContent.trim()) return;
    try {
      await api.editMessage(serverId, message.id, editContent.trim());
    } catch {
      // ignore
    }
    setEditing(false);
  }

  return (
    <div
      id={`msg-${message.id}`}
      className={`message${grouped ? " grouped" : ""}${highlighted ? " search-highlight" : ""}`}
      onContextMenu={(e) => {
        e.preventDefault();
        if (!deleted) setContextMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      {grouped && (
        <span className="grouped-timestamp">
          {new Date(message.timestamp * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}
        </span>
      )}
      {!grouped && (
        <div className="message-header">
          <span className="message-avatar">{displayName.charAt(0).toUpperCase()}</span>
          <span
            className="message-author"
            style={{ color, cursor: member ? "pointer" : undefined }}
            onClick={member ? (e) => setProfilePopup({ x: e.clientX, y: e.clientY }) : undefined}
            onContextMenu={member ? (e) => {
              e.preventDefault();
              e.stopPropagation();
              setMemberMenu({ x: e.clientX, y: e.clientY });
            } : undefined}
          >
            {displayName}
          </span>
          {showModBadges && member && (
            <TimedOutBadge untilMs={member.timeout_until} reason={member.timeout_reason} />
          )}
          <span className="message-timestamp">{formatTimestamp(message.timestamp)}</span>
          {message.edited_at && <span className="message-edited">(edited)</span>}
        </div>
      )}
      {profilePopup && member && (
        <UserProfilePopup
          member={member}
          roles={roles}
          position={profilePopup}
          onClose={() => setProfilePopup(null)}
          isSelf={ownPk === pkStr}
          serverId={serverId}
        />
      )}
      {memberMenu && member && (
        <MemberContextMenu
          target={member}
          serverId={serverId}
          position={memberMenu}
          ownPk={ownPk}
          onClose={() => setMemberMenu(null)}
        />
      )}
      {message.reply_to && (
        <div className="message-reply-context">
          Replying to a message
        </div>
      )}
      {(deleted || displayContent) && (
        <div className={`message-content${deleted ? " deleted-content" : ""}`}>
          {deleted ? (
            <em>This message has been deleted.</em>
          ) : editing ? (
            <div className="message-edit-area">
              <textarea
                className="message-edit-input"
                value={editContent}
                onChange={(e) => setEditContent(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); handleSaveEdit(); }
                  if (e.key === "Escape") setEditing(false);
                }}
                autoFocus
              />
              <div style={{ display: "flex", gap: 4, fontSize: 10 }}>
                <span style={{ cursor: "pointer", color: "var(--xp-link)" }} onClick={handleSaveEdit}>save</span>
                <span style={{ cursor: "pointer", color: "var(--xp-text-muted)" }} onClick={() => setEditing(false)}>cancel</span>
              </div>
            </div>
          ) : (
            <RenderedMessageContent
              text={displayContent}
              attachments={message.attachments}
              bookIndex={bookIndex}
              serverId={serverId}
              renderTextSegment={(t) => renderContent(t, memberNames, ownDisplayName)}
              renderRemainingAttachments={(remaining) =>
                remaining.length > 0 ? (
                  <div className="message-attachments">
                    {remaining.map((att) => (
                      <AttachmentDisplay
                        key={att.id}
                        attachment={att}
                        messageContent={message.content}
                        serverId={serverId}
                      />
                    ))}
                  </div>
                ) : null
              }
            />
          )}
        </div>
      )}

      <TranslatedRow
        messageId={String(message.id)}
        content={message.content}
        defaultTarget={translationSettings?.default_target ?? "en"}
        authorPublicKeyHex={pkStr}
        confirmDownload={async (pair) =>
          new Promise<void>((resolve, reject) => {
            setPendingDownload({ pair, resolve, reject, inProgress: false });
          })
        }
      />

      <div className={`reaction-bar${message.reactions.length === 0 ? " hover-only" : ""}`}>
        {message.reactions.map((r) => (
          <ReactionBadge
            key={`${r.emoji}-${r.file_id ?? "u"}`}
            serverId={serverId}
            reaction={r}
            onClick={() => handleReactionClick(r.emoji, r.me, r.file_id)}
          />
        ))}
        <div style={{ position: "relative" }}>
          <button
            className="reaction-add-btn"
            onClick={() => setShowPicker((p) => !p)}
            title="Add reaction"
          >
            +
          </button>
          {showPicker && (
            <ReactionPicker
              onSelect={handlePickerSelect}
              onSelectBookItem={handlePickerBookSelect}
            />
          )}
        </div>
      </div>

      {message.thread_id !== null && (
        <div className="thread-link" onClick={handleThreadClick}>
          &gt; {message.thread_message_count > 0 ? `${message.thread_message_count} replies` : "View thread"}
        </div>
      )}

      {contextMenu && (
        <>
          <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={() => setContextMenu(null)} />
          <div className="context-menu" style={{ top: contextMenu.y, left: contextMenu.x }}>
            <div className="context-menu-item" onClick={() => {
              if (onReply) onReply(message);
              setContextMenu(null);
            }}>Reply</div>
            {isOwnMessage && (
              <div className="context-menu-item" onClick={() => {
                setEditing(true);
                setEditContent(message.content);
                setContextMenu(null);
              }}>Edit Message</div>
            )}
            <div className="context-menu-item" onClick={() => {
              navigator.clipboard.writeText(message.content);
              setContextMenu(null);
            }}>Copy Text</div>
            {translationSettings?.enabled && (
              <div className="context-menu-item" onClick={async () => {
                setContextMenu(null);
                if (!translationSettings) return;
                await translateMessage({
                  messageId: String(message.id),
                  content: message.content,
                  defaultTarget: translationSettings.default_target,
                  authorPublicKeyHex: pkStr,
                  confirmDownload: async (pair) =>
                    new Promise<void>((resolve, reject) => {
                      setPendingDownload({ pair, resolve, reject, inProgress: false });
                    }),
                });
              }}>Translate</div>
            )}
            {!message.thread_id && (
              <div className="context-menu-item" onClick={async () => {
                try { await api.createThread(serverId, message.id); } catch {}
                setContextMenu(null);
              }}>Create Thread</div>
            )}
            {isOwnMessage && (
              <div className="context-menu-item delete" onClick={async () => {
                try { await api.deleteMessage(serverId, message.id); } catch {}
                setContextMenu(null);
              }}>Delete Message</div>
            )}
          </div>
        </>
      )}
      {pendingDownload && (
        <TranslationDownloadDialog
          pair={pendingDownload.pair}
          inProgress={pendingDownload.inProgress}
          onCancel={() => {
            pendingDownload.reject(new Error("user cancelled"));
            setPendingDownload(null);
            // Clear the translation row too — the user explicitly opted out,
            // so a lingering "Translation failed: user cancelled" error is wrong.
            dismissTranslation(String(message.id));
          }}
          onConfirm={() => {
            setPendingDownload((prev) => (prev ? { ...prev, inProgress: true } : null));
            pendingDownload.resolve();
          }}
        />
      )}
      {pendingDownload && pendingDownload.inProgress && (
        <DismissDialogWhenDone
          messageId={String(message.id)}
          onDone={() => setPendingDownload(null)}
        />
      )}
    </div>
  );
}

function DismissDialogWhenDone({ messageId, onDone }: { messageId: string; onDone: () => void }) {
  useEffect(() => {
    let active = true;
    const unsub = subscribeTranslation((m) => {
      if (!active) return;
      const s = m.get(messageId);
      if (s && (s.kind === "translating" || s.kind === "done" || s.kind === "error")) {
        onDone();
      }
    });
    return () => { active = false; unsub(); };
  }, [messageId, onDone]);
  return null;
}

function AttachmentDisplay({ attachment, messageContent, serverId }: { attachment: AttachmentInfo; messageContent: string; serverId: string }) {
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const isImage = attachment.mime_type.startsWith("image/");
  const isAudio = attachment.mime_type.startsWith("audio/");

  useEffect(() => {
    if (!isImage && !isAudio) return;
    const cached = imageCache.get(attachment.file_id);
    if (cached) {
      setImageUrl(cached);
      return;
    }
    setLoading(true);
    api.downloadFile(serverId, attachment.file_id).then((r) => {
      if (r.data_url) {
        imageCache.set(attachment.file_id, r.data_url);
        setImageUrl(r.data_url);
      }
    }).catch(() => {}).finally(() => setLoading(false));
  }, [attachment.file_id, isImage, isAudio, serverId]);

  async function handleSaveToBook() {
    const defaultName = (attachment.name ?? "").replace(/\.[^.]+$/, "");
    const name = window.prompt(`Save "${attachment.name ?? "image"}" to your book. Name it:`, defaultName);
    if (!name) return;
    try {
      await bookApi.bookSaveFromUrl(serverId, attachment.file_id, name);
    } catch (e) {
      console.error("[book:save-from-chat] failed:", e);
    }
    setMenu(null);
  }

  async function handleSave() {
    setDownloading(true);
    try {
      const result = await api.downloadFile(serverId, attachment.file_id);
      if (result.saved_path) alert(`Saved to ${result.saved_path}`);
    } catch (e) {
      alert(`Download failed: ${e}`);
    } finally {
      setDownloading(false);
      setMenu(null);
    }
  }

  function handleCopyLink() {
    const urlMatch = messageContent.match(/https?:\/\/[^\s]+/);
    if (urlMatch) navigator.clipboard.writeText(urlMatch[0]);
    setMenu(null);
  }

  if ((isImage || isAudio) && loading) {
    return <div className="attachment-loading">Loading {isAudio ? "audio" : "image"}...</div>;
  }

  if (isAudio && imageUrl) {
    return (
      <div className="attachment-audio">
        <audio src={imageUrl} controls style={{ width: "100%", maxWidth: 300, height: 32 }} />
        <div className="attachment-name">{attachment.name} ({formatSize(attachment.size)})</div>
      </div>
    );
  }

  if (isImage && imageUrl) {
    return (
      <div className="attachment-image">
        <img
          src={imageUrl}
          alt={attachment.name}
          onClick={(e) => setMenu({ x: e.clientX, y: e.clientY })}
          onContextMenu={(e) => {
            e.preventDefault();
            setMenu({ x: e.clientX, y: e.clientY });
          }}
          style={{ cursor: "pointer", maxWidth: 400, maxHeight: 300, borderRadius: 3 }}
        />
        <div className="attachment-name">{attachment.name} ({formatSize(attachment.size)})</div>
        {menu && (
          <>
            <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={() => setMenu(null)} />
            <div className="context-menu" style={{ top: menu.y, left: menu.x }}>
              <div className="context-menu-item" onClick={handleCopyLink}>Copy Image Link</div>
              <div className="context-menu-item" onClick={async () => {
                try {
                  const urlMatch = messageContent.match(/https?:\/\/[^\s]+/);
                  await api.addFavorite(serverId, attachment.file_id, urlMatch?.[0]);
                } catch {}
                setMenu(null);
              }}>Favorite</div>
              <div className="context-menu-item" onClick={() => { void handleSaveToBook(); }}>Save to book</div>
            </div>
          </>
        )}
      </div>
    );
  }

  return (
    <div className="attachment-item" onClick={handleSave} style={{ cursor: "pointer" }}>
      <span>[file]</span>
      <span>{attachment.name} ({formatSize(attachment.size)})</span>
      {downloading && <span> downloading...</span>}
    </div>
  );
}

function ReactionBadge({
  serverId,
  reaction,
  onClick,
}: {
  serverId: string;
  reaction: { emoji: string; count: number; me: boolean; file_id?: number };
  onClick: () => void;
}) {
  const [imageUrl, setImageUrl] = useState<string | null>(null);

  useEffect(() => {
    if (reaction.file_id == null) return;
    const fileId = reaction.file_id;
    const cached = imageCache.get(fileId);
    if (cached) {
      setImageUrl(cached);
      return;
    }
    let cancelled = false;
    api.downloadFile(serverId, fileId).then((r) => {
      if (!cancelled && r.data_url) {
        imageCache.set(fileId, r.data_url);
        setImageUrl(r.data_url);
      }
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [reaction.file_id, serverId]);

  return (
    <button
      className={`reaction${reaction.me ? " me" : ""}`}
      onClick={onClick}
      title={`${reaction.emoji === ":custom:" ? "" : reaction.emoji} ${reaction.count}`}
    >
      {reaction.file_id != null ? (
        imageUrl ? (
          <img src={imageUrl} alt="reaction" style={{ width: 18, height: 18, verticalAlign: "middle" }} />
        ) : (
          <span style={{ fontSize: 10 }}>…</span>
        )
      ) : (
        renderUnicodeEmoji(reaction.emoji)
      )}
      <span className="reaction-count">{reaction.count}</span>
    </button>
  );
}
