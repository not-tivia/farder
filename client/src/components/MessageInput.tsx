import { useEffect, useState, useRef, KeyboardEvent } from "react";
import * as api from "../lib/tauri-bridge";
import type { AttachmentCapInput } from "../lib/tauri-bridge";
import * as bookApi from "../lib/book/client";
import * as gifApi from "../lib/gifSearch";
import type { MemberInfo, CommandInfo } from "../lib/types";
import type { BookItem } from "../lib/book/types";
import { publicKeyToString, isE2eeChannel } from "../lib/types";
import { useActiveServer, useApp } from "../context/ServerContext";
import SendStickerPicker from "./SendStickerPicker";
import EmojiAutocomplete from "./EmojiAutocomplete";
import VoiceRecorder from "./VoiceRecorder";
import BookBrowser from "./BookBrowser";
import GifPicker from "./GifPicker";
import GifSearchOptIn from "./GifSearchOptIn";
import TimeoutBanner from "./TimeoutBanner";
import PollBuilderModal from "./PollBuilderModal";
import GiveawayBuilderModal from "./GiveawayBuilderModal";
import EventBuilderModal from "./EventBuilderModal";
import ReminderBuilderModal from "./ReminderBuilderModal";
import { toast } from "../lib/toast";

interface MessageInputProps {
  channelId: number;
  serverId: string;
  replyTo?: number;
  onSent?: () => void;
}

export default function MessageInput({ channelId, serverId, replyTo, onSent }: MessageInputProps) {
  const [content, setContent] = useState("");
  const [sending, setSending] = useState(false);
  const lastTypingSent = useRef(0);
  const [attachedFileId, setAttachedFileId] = useState<number | null>(null);
  const [attachedFileName, setAttachedFileName] = useState<string | null>(null);
  const [attachedCap, setAttachedCap] = useState<AttachmentCapInput | null>(null);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showStickerPicker, setShowStickerPicker] = useState(false);
  const [showVoiceRecorder, setShowVoiceRecorder] = useState(false);
  const [showBook, setShowBook] = useState(false);
  const [showGifPicker, setShowGifPicker] = useState(false);
  const [showGifOptIn, setShowGifOptIn] = useState(false);
  const [showMentions, setShowMentions] = useState(false);
  const [mentionQuery, setMentionQuery] = useState("");
  const [mentionIndex, setMentionIndex] = useState(0);
  const [bookIndex, setBookIndex] = useState<BookItem[]>([]);
  const [autocompleteQuery, setAutocompleteQuery] = useState<string | null>(null);
  const [autocompletePos, setAutocompletePos] = useState<{ x: number; y: number } | null>(null);
  const [commands, setCommands] = useState<CommandInfo[]>([]);
  const [commandIndex, setCommandIndex] = useState(0);
  // Open builder modal for structured command kinds (poll/giveaway/event/reminder).
  const [builder, setBuilder] = useState<{ kind: "poll" | "giveaway" | "event" | "reminder"; trigger: string } | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const textareaWrapperRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    bookApi.bookListItems().then(setBookIndex).catch(() => {});
  }, []);

  useEffect(() => {
    api.listCommands(serverId).then(setCommands).catch(() => {});
  }, [serverId]);

  const activeServer = useActiveServer();
  const { dispatch } = useApp();
  const members = activeServer?.members ?? [];
  const isE2ee = isE2eeChannel(activeServer?.channels.find((c) => c.id === channelId) ?? null);

  const [ownPk, setOwnPk] = useState<string | null>(null);
  useEffect(() => {
    api.getPublicKey().then(setOwnPk).catch(() => {});
  }, []);

  const me = ownPk ? activeServer?.members.find(m => publicKeyToString(m.public_key) === ownPk) ?? null : null;
  const timeoutActive = !!(me?.timeout_until && me.timeout_until > Date.now());

  // Tick once per second while timed out so UI re-renders for the countdown.
  const [, setNowTick] = useState(0);
  useEffect(() => {
    if (!timeoutActive) return;
    const t = setInterval(() => setNowTick(n => n + 1), 1000);
    return () => clearInterval(t);
  }, [timeoutActive]);

  const filteredMembers = members.filter(m =>
    m.display_name.toLowerCase().includes(mentionQuery)
  ).slice(0, 8);

  async function handleAttach() {
    setError(null);
    const path = await api.pickFile();
    if (!path) return;
    const fileName = path.split(/[/\\]/).pop() ?? "file";
    setAttachedFileName(fileName);
    setUploading(true);
    try {
      const outcome = await api.uploadFile(serverId, channelId, path);
      setAttachedFileId(outcome.fileId);
      setAttachedCap({ contentHash: outcome.contentHash, declaredType: outcome.declaredType, size: outcome.size });
    } catch (e) {
      setError(String(e));
      setAttachedFileName(null);
      setAttachedFileId(null);
      setAttachedCap(null);
    } finally {
      setUploading(false);
    }
  }

  function handleRemoveAttachment() {
    setAttachedFileId(null);
    setAttachedFileName(null);
    setAttachedCap(null);
    setError(null);
  }

  async function handleGifButtonClick() {
    try {
      const settings = await gifApi.getGifSearchSettings();
      if (settings.enabled) {
        setShowGifPicker((s) => !s);
      } else {
        setShowGifOptIn(true);
      }
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleGifOptInEnable() {
    try {
      const current = await gifApi.getGifSearchSettings();
      await gifApi.setGifSearchSettings({ ...current, enabled: true });
      setShowGifOptIn(false);
      setShowGifPicker(true);
    } catch (e) {
      setError(String(e));
    }
  }

  function detectTokenAtCursor(text: string, cursor: number): string | null {
    let i = cursor - 1;
    while (i >= 0) {
      const c = text[i];
      if (c === ":") {
        const after = text.slice(i + 1, cursor);
        if (after.length < 2 || /[:\s]/.test(after)) return null;
        if (!/^[a-z0-9_+\-]+$/i.test(after)) return null;
        return after;
      }
      if (/[\s:]/.test(c)) return null;
      i--;
    }
    return null;
  }

  function handleAutocompleteSelect(name: string) {
    const ta = textareaRef.current;
    if (!ta) return;
    const cursor = ta.selectionStart;
    let colonIdx = cursor - 1;
    while (colonIdx >= 0 && content[colonIdx] !== ":") colonIdx--;
    if (colonIdx < 0) return;
    const before = content.slice(0, colonIdx);
    const after = content.slice(cursor);
    const inserted = `:${name}: `;
    const next = before + inserted + after;
    setContent(next);
    setAutocompleteQuery(null);
    setAutocompletePos(null);
    setTimeout(() => {
      ta.focus();
      const newCursor = colonIdx + inserted.length;
      ta.setSelectionRange(newCursor, newCursor);
    }, 0);
  }

  async function resolveInlineEmojiAttachments(text: string, currentAttachments: number[]): Promise<number[]> {
    const tokens: string[] = [];
    const re = /:([a-z0-9_+\-]+):/gi;
    let m;
    while ((m = re.exec(text)) !== null) {
      tokens.push(m[1].toLowerCase());
    }
    const out = [...currentAttachments];
    const seen = new Set<string>();
    for (const name of tokens) {
      if (seen.has(name)) continue;
      seen.add(name);
      const item = bookIndex.find((b) => b.name === name);
      if (!item) continue;
      try {
        const fileId = await bookApi.bookGetFileForServer(serverId, item.id);
        if (!out.includes(fileId)) out.push(fileId);
      } catch (e) {
        console.error("[message-input:inline-emoji]", name, e);
      }
    }
    return out;
  }

  async function handleVoiceRecorded(filePath: string, _duration: number) {
    setShowVoiceRecorder(false);
    setSending(true);
    try {
      // Upload the WAV file via existing system
      const outcome = await api.uploadFile(serverId, channelId, filePath);

      // Send message with attachment
      await api.sendMessage(serverId, channelId, "", undefined, [outcome.fileId]);
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  }

  const imageUrlRegex = /https?:\/\/[^\s]+\.(?:png|jpg|jpeg|gif|webp)(?:\?[^\s]*)?/gi;

  async function handleSend() {
    const text = content.trim();
    if ((!text && !attachedFileId) || sending) return;

    if (text.startsWith("/")) {
      const [word, ...rest] = text.slice(1).split(/\s+/);
      const cmd = commands.find(c => c.trigger === word.toLowerCase());
      if (cmd) {
        const args = rest.join(" ");
        // Builder kinds with no args: open the form instead of sending (the
        // server would just reject an empty arg string with a usage error).
        // Non-empty args keep the direct power-user path below.
        if ((cmd.kind === "poll" || cmd.kind === "giveaway" || cmd.kind === "event" || cmd.kind === "reminder") && !args.trim()) {
          setBuilder({ kind: cmd.kind, trigger: cmd.trigger });
          return;
        }
        setSending(true); setError(null);
        try {
          // Notice-returning commands (/remind) post nothing to the channel, so
          // this toast is the ONLY confirmation the typed power-user path gets —
          // same contract as the builder's handleBuilderCreated.
          const notice = await api.runCommand(serverId, cmd.trigger, channelId, args);
          if (notice) toast.success(notice);
          setContent("");
        } catch (e) { setError(String(e)); }        // RunCommand Error shown to invoker; no channel post
        finally { setSending(false); }
        return;
      }
      // unknown /word -> fall through and send as a normal message
    }

    // Sealed branch (T10): an E2EE channel routes to the seal-and-submit
    // command instead of the legacy submitEvent/sendMessage paths. The legacy
    // fetchUrl image-embed, DM encryption, inline :emoji: book-item resolution,
    // and attachment paths are all EXCLUDED here — a sealed message is text-only
    // this rung (sub-6 owns attachments) and replies stay top-level (a numeric
    // replyTo is not mapped to an event-hash ref yet).
    if (isE2ee) {
      const logServerId = activeServer?.logServerId ?? null;
      if (!logServerId) {
        setError("This channel is encrypted but the server has no log id");
        return;
      }
      setSending(true);
      setError(null);
      try {
        // Record what we typed against the event hash the server assigns. The
        // author cannot decrypt their own MLS message, so this is the ONLY way
        // their own text ever renders — see `ownSealedSends`.
        const sent = await api.sendSealedMessage(serverId, logServerId, channelId, text, null);
        dispatch({
          type: "OWN_SEALED_SENT",
          serverId,
          payload: { eventHash: sent.event_hash, content: text },
        });
        setContent("");
        setAttachedFileId(null);
        setAttachedFileName(null);
        setAttachedCap(null);
        if (onSent) onSent();
      } catch (e) {
        setError(String(e));
      } finally {
        setSending(false);
      }
      return;
    }

    setSending(true);
    setError(null);
    try {
      const attachments: number[] = attachedFileId ? [attachedFileId] : [];

      // Auto-fetch image URLs found in the message text (legacy-only: no client-known hash).
      const urls = text.match(imageUrlRegex) || [];
      let hasUncappableAttachment = urls.length > 0;
      for (const url of urls) {
        try {
          const fileId = await api.fetchUrl(serverId, url, channelId);
          attachments.push(fileId);
        } catch {
          // Failed to fetch — leave the URL as plain text
        }
      }

      // Encrypt the message content if this is a DM channel
      let messageContent = text;
      const dm = activeServer?.dms.find(d => d.channel.id === channelId);
      if (dm) {
        const peerPk = publicKeyToString(dm.participant.public_key);
        try {
          messageContent = await api.dmEncrypt(peerPk, text);
        } catch {
          setError("Encryption failed — message not sent");
          return;
        }
      }

      // Resolve inline :name: tokens into book-item file_ids (legacy-only: no client cap).
      const beforeEmoji = attachments.length;
      const finalAttachments = await resolveInlineEmojiAttachments(text, attachments);
      if (finalAttachments.length > beforeEmoji) hasUncappableAttachment = true;

      const logServerId = activeServer?.logServerId ?? null;
      // Route over the mesh log when this is a log server, not a DM, and every
      // attachment carries a client-known cap (i.e. only the staged upload — URL
      // images and inline emoji have no client-side hash and stay on legacy).
      if (logServerId && !dm && !hasUncappableAttachment) {
        const caps = attachedCap ? [attachedCap] : [];
        // TODO(mesh): replies over the log need event-hash mapping; legacy replyTo is a numeric id, so drop it for now (top-level post).
        await api.submitEvent(serverId, logServerId, channelId, messageContent, null, caps);
      } else {
        await api.sendMessage(
          serverId,
          channelId,
          messageContent,
          replyTo,
          finalAttachments.length > 0 ? finalAttachments : undefined,
        );
      }
      setContent("");
      setAttachedFileId(null);
      setAttachedFileName(null);
      setAttachedCap(null);
      if (onSent) onSent();
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  }

  // Derived: slash command autocomplete prefix and filtered list.
  // Active when content starts with "/" and contains no space yet.
  const commandPrefix =
    content.startsWith("/") && !content.includes(" ")
      ? content.slice(1).toLowerCase()
      : null;
  const filteredCommands =
    commandPrefix !== null
      ? commands.filter((c) => c.trigger.startsWith(commandPrefix))
      : [];

  function insertCommand(cmd: CommandInfo) {
    // Builder kinds: selecting from the "/" autocomplete opens the matching
    // builder modal instead of inserting the trigger text — the modal builds
    // the pipe/token syntax and calls runCommand itself.
    if (cmd.kind === "poll" || cmd.kind === "giveaway" || cmd.kind === "event" || cmd.kind === "reminder") {
      setBuilder({ kind: cmd.kind, trigger: cmd.trigger });
      setCommandIndex(0);
      return;
    }
    setContent("/" + cmd.trigger + " ");
    setCommandIndex(0);
    setTimeout(() => { textareaRef.current?.focus(); }, 0);
  }

  function handleBuilderCreated(notice?: string | null) {
    // Reminder creation posts nothing, so its only confirmation is the server's
    // Notice text (poll/giveaway/event pass nothing -> no toast).
    if (notice) toast.success(notice);
    setBuilder(null);
    // Clear the input if it still holds the slash text that opened the builder.
    setContent((c) => (c.trim().startsWith("/") ? "" : c));
    setTimeout(() => { textareaRef.current?.focus(); }, 0);
    if (onSent) onSent();
  }

  function insertMention(member: MemberInfo) {
    const cursorPos = textareaRef.current?.selectionStart ?? content.length;
    const textBeforeCursor = content.slice(0, cursorPos);
    const textAfterCursor = content.slice(cursorPos);
    const atPos = textBeforeCursor.lastIndexOf("@");
    const newText = textBeforeCursor.slice(0, atPos) + `@${member.display_name} ` + textAfterCursor;
    setContent(newText);
    setShowMentions(false);
  }

  function handleChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    const val = e.target.value;
    setContent(val);
    setCommandIndex(0);

    // Detect @mention
    const cursorPos = e.target.selectionStart ?? val.length;
    const textBeforeCursor = val.slice(0, cursorPos);
    const atMatch = textBeforeCursor.match(/@(\w*)$/);

    if (atMatch) {
      setMentionQuery(atMatch[1].toLowerCase());
      setShowMentions(true);
      setMentionIndex(0);
    } else {
      setShowMentions(false);
    }

    // Throttled typing indicator
    const now = Date.now();
    if (now - lastTypingSent.current > 5000 && val.trim()) {
      lastTypingSent.current = now;
      api.sendTyping(serverId, channelId).catch(() => {});
    }

    // Token autocomplete detection.
    const query = detectTokenAtCursor(val, cursorPos);
    setAutocompleteQuery(query);
    if (query) {
      const rect = textareaWrapperRef.current?.getBoundingClientRect();
      if (rect) {
        setAutocompletePos({ x: rect.left, y: rect.top - 4 });
      }
    } else {
      setAutocompletePos(null);
    }
  }

  function handleKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (filteredCommands.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setCommandIndex((prev) => Math.min(prev + 1, filteredCommands.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setCommandIndex((prev) => Math.max(prev - 1, 0));
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
        e.preventDefault();
        insertCommand(filteredCommands[Math.min(commandIndex, filteredCommands.length - 1)]);
        return;
      }
    }
    if (showMentions && filteredMembers.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMentionIndex(prev => Math.min(prev + 1, filteredMembers.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMentionIndex(prev => Math.max(prev - 1, 0));
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
        e.preventDefault();
        insertMention(filteredMembers[mentionIndex]);
        return;
      }
      if (e.key === "Escape") {
        setShowMentions(false);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  return (
    <div className="message-input-area">
      <div className="message-input-wrapper">
        {showMentions && filteredMembers.length > 0 && (
          <div className="mention-autocomplete">
            {filteredMembers.map((m, i) => (
              <div
                key={publicKeyToString(m.public_key)}
                className={`mention-autocomplete-item${i === mentionIndex ? " active" : ""}`}
                onMouseDown={(e) => { e.preventDefault(); insertMention(m); }}
              >
                <span className="mention-avatar">{(m.display_name || "?").charAt(0).toUpperCase()}</span>
                <span>{m.display_name}</span>
              </div>
            ))}
          </div>
        )}
        {(attachedFileName || uploading) && (
          <div className="attachment-preview">
            {uploading ? (
              <span>Uploading...</span>
            ) : (
              <>
                <span className="attachment-preview-name">{attachedFileName}</span>
                <button className="attachment-remove-btn" onClick={handleRemoveAttachment} title="Remove attachment">
                  x
                </button>
              </>
            )}
          </div>
        )}
        {error && <div className="error-text" style={{ padding: "2px 4px" }}>{error}</div>}
        {me?.timeout_until != null && me.timeout_until > Date.now() && (
          <TimeoutBanner untilMs={me.timeout_until} reason={me.timeout_reason ?? null} />
        )}
        {showVoiceRecorder ? (
          <VoiceRecorder
            onRecorded={handleVoiceRecorded}
            onCancel={() => setShowVoiceRecorder(false)}
          />
        ) : (
          <div className="message-input-row">
            <div style={{ position: "relative" }}>
              <button
                className="xp-button attach-btn"
                onClick={() => setShowStickerPicker((s) => !s)}
                disabled={sending}
                title="Send Sticker"
              >
                🎁
              </button>
              {showStickerPicker && (
                <SendStickerPicker
                  serverId={serverId}
                  channelId={channelId}
                  onClose={() => setShowStickerPicker(false)}
                />
              )}
            </div>
            <div style={{ position: "relative" }}>
              <button
                className="xp-button attach-btn"
                onClick={handleGifButtonClick}
                disabled={sending}
                title="GIF Search"
              >
                🎬
              </button>
              {showGifPicker && (
                <GifPicker
                  serverId={serverId}
                  channelId={channelId}
                  onClose={() => setShowGifPicker(false)}
                />
              )}
            </div>
            <button
              className="xp-button attach-btn"
              onClick={handleAttach}
              disabled={sending || uploading}
              title="Attach file"
            >
              +
            </button>
            <button
              className="xp-button attach-btn"
              onClick={() => setShowVoiceRecorder(true)}
              disabled={sending}
              title="Voice Message"
            >
              Mic
            </button>
            <div ref={textareaWrapperRef} style={{ position: "relative", flex: 1 }}>
              {filteredCommands.length > 0 && (
                <div className="mention-autocomplete">
                  {filteredCommands.map((cmd, i) => (
                    <div
                      key={cmd.id}
                      className={`mention-autocomplete-item${i === Math.min(commandIndex, filteredCommands.length - 1) ? " active" : ""}`}
                      onMouseDown={(e) => { e.preventDefault(); insertCommand(cmd); }}
                    >
                      <span style={{ fontWeight: 600 }}>/{cmd.trigger}</span>
                      <span style={{ color: "var(--xp-text-muted)", marginLeft: 6 }}>{cmd.description}</span>
                    </div>
                  ))}
                </div>
              )}
              <textarea
                ref={textareaRef}
                className={`message-input${isE2ee ? " e2ee" : ""}`}
                value={content}
                onChange={handleChange}
                onKeyDown={handleKeyDown}
                placeholder={isE2ee ? "Encrypted message…" : "Type a message… (Enter to send, Shift+Enter for newline)"}
                rows={1}
                disabled={sending || timeoutActive}
                style={{ width: "100%", boxSizing: "border-box" }}
              />
            </div>
            <button
              className="xp-button attach-btn"
              onClick={() => setShowBook(true)}
              disabled={sending}
              title="Reaction Book"
            >
              📚
            </button>
            <button
              className="xp-button"
              onClick={handleSend}
              disabled={sending || uploading || timeoutActive || (!content.trim() && !attachedFileId)}
            >
              Send
            </button>
          </div>
        )}
      </div>
      {showBook && <BookBrowser onClose={() => setShowBook(false)} />}
      {builder?.kind === "poll" && (
        <PollBuilderModal
          serverId={serverId}
          channelId={channelId}
          trigger={builder.trigger}
          onClose={() => setBuilder(null)}
          onCreated={handleBuilderCreated}
        />
      )}
      {builder?.kind === "giveaway" && (
        <GiveawayBuilderModal
          serverId={serverId}
          channelId={channelId}
          trigger={builder.trigger}
          onClose={() => setBuilder(null)}
          onCreated={handleBuilderCreated}
        />
      )}
      {builder?.kind === "event" && (
        <EventBuilderModal
          serverId={serverId}
          channelId={channelId}
          trigger={builder.trigger}
          onClose={() => setBuilder(null)}
          onCreated={handleBuilderCreated}
        />
      )}
      {builder?.kind === "reminder" && (
        <ReminderBuilderModal
          serverId={serverId}
          channelId={channelId}
          trigger={builder.trigger}
          onClose={() => setBuilder(null)}
          onCreated={handleBuilderCreated}
        />
      )}
      {showGifOptIn && (
        <GifSearchOptIn
          onCancel={() => setShowGifOptIn(false)}
          onEnable={handleGifOptInEnable}
        />
      )}
      {autocompleteQuery !== null && autocompletePos && (
        <EmojiAutocomplete
          query={autocompleteQuery}
          bookIndex={bookIndex}
          position={autocompletePos}
          onSelect={handleAutocompleteSelect}
          onClose={() => { setAutocompleteQuery(null); setAutocompletePos(null); }}
        />
      )}
    </div>
  );
}
