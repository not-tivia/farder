import { useState, useEffect, useMemo, useRef } from "react";
import type { MessageInfo, AttachmentInfo } from "../lib/types";
import { publicKeyToString, isDeletedUser, memberDisplayName } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { toast } from "../lib/toast";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { useApp, useActiveServer } from "../context/ServerContext";
import ReactionPicker from "./ReactionPicker";
import UserProfilePopup from "./UserProfilePopup";
import MemberContextMenu from "./MemberContextMenu";
import RenderedMessageContent from "./RenderedMessageContent";
import TimedOutBadge from "./TimedOutBadge";
import { getActorPermissions, isModerator, hasPermission, PERMISSIONS } from "../lib/permissions";
import { renderUnicodeEmoji } from "../lib/unicodeEmoji";
import { TranslatedRow } from "./TranslatedRow";
import { TranslationDownloadDialog } from "./TranslationDownloadDialog";
import { translateMessage, subscribe as subscribeTranslation, dismiss as dismissTranslation } from "../lib/translation/store";
import { getTranslationSettings } from "../lib/translation/api";
import InviteEmbed from "./InviteEmbed";
import { parseInviteLink } from "../lib/invite";
import LinkEmbed from "./LinkEmbed";
import { detectEmbedUrls } from "../lib/linkEmbed";
import PollWidget from "./PollWidget";
import MemberAvatar from "./MemberAvatar";
import { useDataSaver } from "../context/DataSaverContext";
import { imageIsGated } from "../lib/dataSaver";
import { useClickAnchoredPosition } from "../lib/useClickAnchoredPosition";

const INVITE_REGEX = /(?:https?:\/\/)?farder\.gg\/join\/[A-Za-z0-9_-]+|farder:\/\/[^\s]+/gi;

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

// Capturing-group variant of INVITE_REGEX: split() keeps the matched links.
// Fresh non-global tester per call below — a shared /g regex is stateful.
const INVITE_SPLIT_REGEX = /((?:https?:\/\/)?farder\.gg\/join\/[A-Za-z0-9_-]+|farder:\/\/[^\s]+)/gi;

function isInviteLink(s: string): boolean {
  return /^(?:(?:https?:\/\/)?farder\.gg\/join\/[A-Za-z0-9_-]+|farder:\/\/[^\s]+)$/i.test(s);
}

function copyInviteLink(link: string) {
  navigator.clipboard?.writeText(link).then(
    () => toast.success("Invite link copied"),
    () => {},
  );
}

function renderMentions(text: string, memberNames: Record<string, string>, ownDisplayName: string | null) {
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

function renderContent(text: string, memberNames: Record<string, string>, ownDisplayName: string | null) {
  // Farder invite URLs display as a compact pill (the invite card below the
  // message carries the details); clicking the pill copies the full link.
  // The underlying message content is untouched — only the display changes.
  const segments = text.split(INVITE_SPLIT_REGEX);
  return segments.map((seg, si) => {
    if (seg && isInviteLink(seg)) {
      return (
        <span
          key={`pill-${si}`}
          className="invite-link-pill"
          title={seg}
          onClick={(e) => {
            e.stopPropagation();
            copyInviteLink(seg);
          }}
        >
          {"\u2709 Server invite"}
        </span>
      );
    }
    return <span key={`seg-${si}`}>{renderMentions(seg, memberNames, ownDisplayName)}</span>;
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
  // Widget fallback signal: when the widget's data can't be fetched (deleted/
  // unknown), the card degrades to its plain-text content.
  const [widgetUnavailable, setWidgetUnavailable] = useState(false);
  const contextMenuRef = useRef<HTMLDivElement | null>(null);
  const contextMenuPos = useClickAnchoredPosition(contextMenuRef, contextMenu ?? { x: 0, y: 0 }, { anchor: "auto" });
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
    user_language_overrides: Record<string, string>;
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
      setTranslationSettings({
        enabled: s.enabled,
        default_target: s.default_target,
        user_language_overrides: s.user_language_overrides,
      })
    );
  }, []);

  const deleted = isDeletedUser(message.author);
  const pkStr = publicKeyToString(message.author);
  const displayName = message.author_name_override
    ? message.author_name_override
    : deleted
      ? "Deleted User"
      : (memberNames[pkStr] != null ? memberDisplayName(memberNames[pkStr]) : pkStr.slice(0, 16) + "…");
  const color = deleted ? "#999" : authorColor(pkStr);
  const member = deleted ? null : (activeServer?.members.find((m) => publicKeyToString(m.public_key) === pkStr) ?? null);
  const roles = activeServer?.roles ?? [];

  // Auto-translate: when this message's author has a per-user language
  // override set (different from the target), fire translateMessage on mount.
  // The store's idempotency guard prevents re-translation on re-renders.
  useEffect(() => {
    if (!translationSettings?.enabled || !serverId) return;
    const override = translationSettings.user_language_overrides[pkStr];
    if (!override || override === translationSettings.default_target) return;
    translateMessage({
      messageId: String(message.id),
      content: message.content,
      defaultTarget: translationSettings.default_target,
      authorPublicKeyHex: pkStr,
      confirmDownload: async (pair) =>
        new Promise<void>((resolve, reject) => {
          setPendingDownload({ pair, resolve, reject, inProgress: false });
        }),
    });
  }, [translationSettings, pkStr, message.id, message.content, serverId]);

  const isOwnMessage = ownPk === pkStr;

  // Build the list of message actions mirroring the context menu (same conditions),
  // so AttachmentDisplay can append them to the merged image right-click menu.
  // NOTE: these are intentionally plain onClick callbacks, not async -- the async
  // bodies are handled inside the callbacks via void/catch patterns matching above.
  const messageActions: { label: string; onClick: () => void }[] = [
    ...(onReply ? [{ label: "Reply", onClick: () => { if (onReply) onReply(message); } }] : []),
    ...(isOwnMessage ? [{ label: "Edit Message", onClick: () => { setEditing(true); setEditContent(message.content); } }] : []),
    { label: "Copy Text", onClick: () => { navigator.clipboard.writeText(message.content); } },
    ...(translationSettings?.enabled ? [{
      label: "Translate",
      onClick: () => {
        if (!translationSettings) return;
        void translateMessage({
          messageId: String(message.id),
          content: message.content,
          defaultTarget: translationSettings.default_target,
          authorPublicKeyHex: pkStr,
          confirmDownload: async (pair) =>
            new Promise<void>((resolve, reject) => {
              setPendingDownload({ pair, resolve, reject, inProgress: false });
            }),
        });
      },
    }] : []),
    ...(!message.thread_id ? [{
      label: "Create Thread",
      onClick: () => {
        void api.createThread(serverId, message.id).catch((e) => { toast.error(`Couldn't create thread: ${e}`); });
      },
    }] : []),
    ...(isOwnMessage ? [{
      label: "Delete Message",
      onClick: () => {
        void api.deleteMessage(serverId, message.id).catch((e) => { toast.error(`Couldn't delete message: ${e}`); });
      },
    }] : []),
  ];

  const { bits: viewerBits } = ownPk
    ? getActorPermissions(activeServer?.members ?? [], roles, ownPk, activeServer?.ownerPublicKey ?? null)
    : { bits: 0n };
  const showModBadges = isModerator(viewerBits);
  const canTakeDown = hasPermission(viewerBits, PERMISSIONS.KICK_MEMBERS);
  const logServerId = activeServer?.logServerId ?? null;

  // Server-written widget marker ({"type":"poll","id":7}); treated as untrusted:
  // try/catch parse, id must be a number. Unknown types fall back to plain content.
  const parsedWidget = useMemo((): { type: string; id: number } | null => {
    if (!message.widget) return null;
    try {
      const p = JSON.parse(message.widget);
      if (p && typeof p.type === "string" && typeof p.id === "number") {
        return { type: p.type, id: p.id };
      }
    } catch {
      // malformed widget JSON → plain content
    }
    return null;
  }, [message.widget]);

  // Widget dispatch: known types render an interactive card IN PLACE OF the
  // .message-content text body (the content string is the old-client fallback).
  const widgetNode = (() => {
    if (!parsedWidget || deleted || widgetUnavailable) return null;
    switch (parsedWidget.type) {
      case "poll":
        return (
          <PollWidget
            serverId={serverId}
            pollId={parsedWidget.id}
            onUnavailable={() => setWidgetUnavailable(true)}
          />
        );
      default:
        return null;
    }
  })();
  // Keep the editor reachable: editing always shows the plain-content branch.
  const showWidget = widgetNode !== null && !editing;

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
    } catch (e) {
      toast.error(`Reaction failed: ${e}`);
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
    } catch (e) {
      toast.error(`Reaction failed: ${e}`);
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
      setEditing(false); // only close the editor once the edit actually saved
    } catch (e) {
      toast.error(`Couldn't save edit: ${e}`);
    }
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
          <MemberAvatar
            className="message-avatar"
            serverId={serverId}
            publicKey={member ? pkStr : undefined}
            profileHash={member?.profile_hash}
            name={displayName || "?"}
          />
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
          {message.author_name_override && (
            <span className="message-webhook-badge">{message.author_badge ?? "WEBHOOK"}</span>
          )}
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
      {/* Attachment-only messages (voice notes, captionless images) have empty
          content — the body must still render so the attachments do. */}
      {(deleted || displayContent || message.attachments.length > 0) && !showWidget && (
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
                        messageActions={messageActions}
                        logServerId={logServerId}
                        isOwnMessage={isOwnMessage}
                        canTakeDown={canTakeDown}
                      />
                    ))}
                  </div>
                ) : null
              }
            />
          )}
        </div>
      )}

      {showWidget && widgetNode}

      {!deleted && (() => {
        const rawMatches = message.content.match(INVITE_REGEX) ?? [];
        const seen = new Set<string>();
        const embeds: string[] = [];
        for (const m of rawMatches) {
          if (embeds.length >= 3) break;
          const parsed = parseInviteLink(m);
          if (!parsed.address) continue;
          if (seen.has(parsed.address)) continue;
          seen.add(parsed.address);
          embeds.push(m);
        }
        return embeds.length > 0 ? (
          <div className="invite-embeds">
            {embeds.map((m, i) => <InviteEmbed key={i} link={m} />)}
          </div>
        ) : null;
      })()}

      {!deleted && (() => {
        const urls = detectEmbedUrls(message.content);
        return urls.length > 0 ? (
          <div className="link-embeds">
            {urls.map((u, i) => <LinkEmbed key={i} url={u} />)}
          </div>
        ) : null;
      })()}

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
          <div ref={contextMenuRef} className="context-menu" style={contextMenuPos}>
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
                try { await api.createThread(serverId, message.id); }
                catch (e) { toast.error(`Couldn't create thread: ${e}`); }
                setContextMenu(null);
              }}>Create Thread</div>
            )}
            {isOwnMessage && (
              <div className="context-menu-item delete" onClick={async () => {
                try { await api.deleteMessage(serverId, message.id); }
                catch (e) { toast.error(`Couldn't delete message: ${e}`); }
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

function AttachmentDisplay({
  attachment,
  messageContent,
  serverId,
  messageActions,
  logServerId,
  isOwnMessage,
  canTakeDown,
}: {
  attachment: AttachmentInfo;
  messageContent: string;
  serverId: string;
  messageActions?: { label: string; onClick: () => void }[];
  logServerId: string | null;
  isOwnMessage: boolean;
  canTakeDown: boolean;
}) {
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; isRightClick: boolean } | null>(null);
  const attachMenuRef = useRef<HTMLDivElement | null>(null);
  const attachMenuPos = useClickAnchoredPosition(attachMenuRef, menu ?? { x: 0, y: 0 }, { anchor: "auto" });
  const isImage = attachment.mime_type.startsWith("image/");
  const isAudio = attachment.mime_type.startsWith("audio/");
  const { settings: ds } = useDataSaver();
  const [userLoaded, setUserLoaded] = useState(false);
  const gated =
    isImage &&
    !userLoaded &&
    !imageCache.has(attachment.file_id) &&
    imageIsGated(ds, attachment.size);

  useEffect(() => {
    if (!isImage && !isAudio) return;
    if (gated) return;
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
  }, [attachment.file_id, isImage, isAudio, serverId, gated]);

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

  if (attachment.redacted_by_moderator != null) {
    const who = attachment.redacted_by_moderator ? "a moderator" : "the uploader";
    return (
      <div className="attachment-item">
        <span>&#x1F6AB;</span>
        <span>Removed by {who}</span>
      </div>
    );
  }

  if (isImage && gated) {
    // Reserve the image's real footprint so loading doesn't shift layout,
    // capped to the same 400x300 max the loaded <img> uses.
    const maxW = 400, maxH = 300;
    let w = attachment.width ?? 0;
    let h = attachment.height ?? 0;
    if (w > 0 && h > 0) {
      const scale = Math.min(1, maxW / w, maxH / h);
      w = Math.round(w * scale);
      h = Math.round(h * scale);
    } else {
      w = 200; h = 150;
    }
    return (
      <div
        className="attachment-image"
        style={{ width: w, height: h, display: "flex", alignItems: "center", justifyContent: "center" }}
      >
        <button className="link-embed-chip" onClick={() => setUserLoaded(true)}>
          &#11015; Load image ({formatSize(attachment.size)})
        </button>
      </div>
    );
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
          onClick={(e) => setMenu({ x: e.clientX, y: e.clientY, isRightClick: false })}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setMenu({ x: e.clientX, y: e.clientY, isRightClick: true });
          }}
          style={{ cursor: "pointer", maxWidth: 400, maxHeight: 300, borderRadius: 3 }}
        />
        <div className="attachment-name">{attachment.name} ({formatSize(attachment.size)})</div>
        {menu && (
          <>
            <div style={{ position: "fixed", inset: 0, zIndex: 999 }} onClick={() => setMenu(null)} />
            <div ref={attachMenuRef} className="context-menu" style={attachMenuPos}>
              <div className="context-menu-item" onClick={handleCopyLink}>Copy Image Link</div>
              {/* "Favorite" was removed: it wrote to a legacy favorites store
                  nothing in the UI reads anymore — the book replaced it. */}
              <div className="context-menu-item" onClick={() => { void handleSaveToBook(); }}>Save to book</div>
              {logServerId && attachment.redacted_by_moderator == null && attachment.content_hash && (isOwnMessage || canTakeDown) && (
                <div className="context-menu-item" onClick={async () => {
                  try { await api.redactAttachment(serverId, logServerId, attachment.content_hash!); }
                  catch (e) { console.error("[attachment:redact]", e); }
                  setMenu(null);
                }}>
                  {isOwnMessage ? "Remove" : "Take down"}
                </div>
              )}
              {menu.isRightClick && messageActions && messageActions.length > 0 && (
                <>
                  <div className="context-menu-divider" />
                  {messageActions.map((action) => (
                    <div
                      key={action.label}
                      className="context-menu-item"
                      onClick={() => { setMenu(null); action.onClick(); }}
                    >
                      {action.label}
                    </div>
                  ))}
                </>
              )}
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
