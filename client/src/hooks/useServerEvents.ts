import { useEffect, useRef } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { useApp, type PerServerState } from "../context/ServerContext";
import type { MessageInfo, ChannelInfo, CategoryInfo, RoleInfo, Presence, PollInfo, GiveawayInfo, EventInfo, MessageInfoV2, ChannelInfoV2 } from "../lib/types";
import { publicKeyToString, flattenMessageInfoV2, flattenChannelInfoV2, isE2eeChannel } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { refreshServerClasses } from "../lib/refreshServerClasses";
import { dismiss as dismissTranslation, clearAll as clearTranslations } from "../lib/translation/store";
import type { NotificationPrefs, AuditEvent } from "../lib/tauri-bridge";
import { getAutoplay, shouldAutoplay, firstDirectMediaUrl, prefersReducedMotion } from "../lib/autoplay";
import { getDataSaver } from "../lib/dataSaver";
import { buildNotificationText, normalizePopupContent } from "../lib/notificationText";
import {
  MIN_REPLY_GAP_MS, REPLY_EVENT, REPLY_RESULT_EVENT, executeReply, planReply,
  type ReplyRequest, type ReplyResult, type ReplyServerView,
} from "../lib/quickReply";
import { getGarble } from "../lib/garbleSettings";
import {
  STRIP_OPEN_EVENT, STRIP_READY_EVENT, STRIP_STATE_EVENT, StripModel, parseOpen,
} from "../lib/edgeStrip";
import { garbleText, makeTransform } from "../lib/garble";

// Module-level cache for notification prefs and own public key
let notifPrefs: NotificationPrefs | null = null;
// How the module-level notification code looks up a server's members/channels
// (set by the hook below, which owns the app state).
let serverLookup: ((serverId: string) => PerServerState | undefined) | null = null;
api.getNotificationPrefs().then(p => { notifPrefs = p; }).catch(() => {});
// The edge strip (a one-line docked bar, see edge_strip.rs). When it is on it
// REPLACES the pop-up: the same messages, the same privacy setting, one line.
const strip = new StripModel();
let stripEnabled = false;
export function refreshStripCache(): void {
  api.getEdgeStrip().then((s) => { stripEnabled = s.enabled; if (!s.enabled) strip.clear(); }).catch(() => {});
}
refreshStripCache();
function publishStrip(): void {
  void emit(STRIP_STATE_EVENT, strip.state()).catch(() => {});
}
// Own public key, fetched LAZILY: at module load the identity is still
// PIN-locked, so an eager getPublicKey() fails and would leave this null for
// the whole session (making every own-reaction event look like someone
// else's — the stacking-reactions bug). A failed fetch retries next call.
let cachedOwnPk: string | null = null;
let ownPkPromise: Promise<string | null> | null = null;
export function getOwnPk(): Promise<string | null> {
  if (cachedOwnPk != null) return Promise.resolve(cachedOwnPk);
  if (!ownPkPromise) {
    ownPkPromise = api
      .getPublicKey()
      .then((pk) => {
        // Only a REAL key is cached. While the identity is still PIN-locked the
        // command answers `null` SUCCESSFULLY, and caching that pinned "we don't
        // know who we are" for the whole session — the same root cause behind
        // the stacking-reactions bug this comment already describes.
        if (pk) cachedOwnPk = pk;
        else ownPkPromise = null;
        return pk;
      })
      .catch(() => {
        ownPkPromise = null; // retry on the next event
        return null;
      });
  }
  return ownPkPromise;
}

function checkMentionsOrKeywords(content: string, prefs: NotificationPrefs): boolean {
  if (prefs.keywords.length > 0) {
    const lower = content.toLowerCase();
    if (prefs.keywords.some(k => lower.includes(k.toLowerCase()))) return true;
  }
  if (prefs.mentionNotifications && content.includes("@")) return true;
  return false;
}

function shouldNotify(serverId: string, message: MessageInfo, prefs: NotificationPrefs): boolean {
  // Check if user is muted
  const authorPk = publicKeyToString(message.author);
  if (prefs.mutedUsers.includes(authorPk)) return false;

  // Check server-specific mode
  const serverPref = prefs.servers[serverId];
  if (serverPref) {
    if (serverPref.mode === "none") return false;
    if (serverPref.mode === "mentions") {
      // Only notify if message contains a mention of us or a keyword
      return checkMentionsOrKeywords(message.content, prefs);
    }
    // "all" — fall through to notify
  }

  // Check keywords
  if (prefs.keywords.length > 0) {
    const lower = message.content.toLowerCase();
    if (prefs.keywords.some(k => lower.includes(k.toLowerCase()))) return true;
  }

  return true; // default: notify
}

/**
 * Raise a pop-up for a message if the person's preferences want one. Never for
 * our own messages (another device of ours can echo one back). Prefs not loaded
 * yet fall back to notifying, as before.
 */
function notifyIfWanted(serverId: string, message: MessageInfo, text: string): void {
  if (!text) { console.log("[notify] skipped: empty text"); return; }
  if (notifPrefs && !shouldNotify(serverId, message, notifPrefs)) { console.log("[notify] skipped: notification settings say no for this server/user"); return; }
  // "Off" means off: do not even build the text.
  if (notifPrefs?.popupStyle === "off") { console.log("[notify] skipped: pop-ups are set to off"); return; }
  void getOwnPk().then((own) => {
    const author = publicKeyToString(message.author);
    if (own && author === own) { console.log("[notify] skipped: message is from this identity"); return; }

    // What the pop-up may say is the person's choice (full / sender only / nothing).
    // Decided here, before the text reaches either pop-up style.
    const server = serverLookup?.(serverId);
    const sender = server?.members.find((m) => publicKeyToString(m.public_key) === author)?.display_name ?? null;
    const channel = server?.channels.find((c) => c.id === message.channel_id)?.name ?? null;
    const shown = buildNotificationText(normalizePopupContent(notifPrefs?.popupContent), { sender, channel, text });

    // A muted preview of a direct video/image link, but only from someone the
    // person has chosen to trust (never under Data Saver / reduced motion) and
    // only when the pop-up is allowed to show the message at all.
    const media = shown.allowMedia && shouldAutoplay(
      getAutoplay(), author,
      { dataSaverOn: getDataSaver().enabled, reducedMotion: prefersReducedMotion() },
      "popup",
    ) ? firstDirectMediaUrl(text) : null;
    // Replying from the pop-up: only with the themed pop-up, only when it shows
    // who the message is from, and only if the person has not turned it off.
    const canReply =
      (notifPrefs?.popupStyle ?? "skinned") === "skinned" &&
      normalizePopupContent(notifPrefs?.popupContent) !== "hidden" &&
      notifPrefs?.popupReply !== false;
    const reply = canReply ? { server_id: serverId, channel_id: message.channel_id } : null;
    if (stripEnabled) {
      const mode = normalizePopupContent(notifPrefs?.popupContent);
      strip.note({
        serverId,
        channelId: message.channel_id,
        mode,
        sender,
        channel,
        isDm: !!server?.dms.some((d) => d.channel.id === message.channel_id),
        title: shown.title,
        body: shown.body,
        // Replying from the strip follows the same rules as from the pop-up,
        // except that it does not need the pop-up style to be the themed one.
        reply: mode !== "hidden" && notifPrefs?.popupReply !== false
          ? { server_id: serverId, channel_id: message.channel_id }
          : null,
      });
      publishStrip();
      console.log("[notify] showing it on the strip");
      return; // the strip replaces the pop-up
    }
    console.log("[notify] showing a pop-up");
    api.showNotification(shown.title, shown.body, media, reply).catch((e) => console.warn("[notify] pop-up failed:", e));
  });
}

/** True when Farder is minimised, behind another program, or on another desktop. */
function appIsInBackground(): boolean {
  const bg = typeof document !== "undefined" && !document.hasFocus();
  if (!bg) console.log("[notify] no pop-up: Farder has keyboard focus and the message is in the channel you are viewing");
  return bg;
}

export function refreshNotifPrefsCache(): void {
  api.getNotificationPrefs().then(p => { notifPrefs = p; }).catch(() => {});
}

interface ReactionAddedPayload {
  server_id: string;
  channel_id: number;
  message_id: number;
  emoji: string;
  public_key: string;
  file_id?: number;
}

interface ReactionRemovedPayload {
  server_id: string;
  channel_id: number;
  message_id: number;
  emoji: string;
  file_id?: number;
}



interface ChannelDeletedPayload {
  server_id: string;
  channel_id: number;
}

interface MessageDeletedPayload {
  server_id: string;
  channel_id: number;
  message_id: number;
}

export function useServerEvents(): void {
  const { state, dispatch } = useApp();
  const activeRef = useRef(state.activeServerId);
  useEffect(() => { activeRef.current = state.activeServerId; }, [state.activeServerId]);

  // Keep a live reference to per-server state for use inside event callbacks
  const stateRef = useRef(state);
  useEffect(() => { stateRef.current = state; }, [state]);
  useEffect(() => {
    serverLookup = (id) => stateRef.current.servers[id];
    return () => { serverLookup = null; };
  }, []);

  useEffect(() => {
    // Each listen() returns a Promise<UnlistenFn>. Cleanup runs synchronously
    // and may fire before those promises resolve — without the cancelled flag
    // the resolved unlisten functions would be pushed onto a discarded array
    // and the listener would leak into the next mount cycle (StrictMode dev).
    // safePush invokes the unlisten fn immediately if cleanup already ran.
    let cancelled = false;
    const unlisten: Array<() => void> = [];
    const safePush = (u: () => void) => {
      if (cancelled) u();
      else unlisten.push(u);
    };

    listen("server:new_message", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      const message = data.message as MessageInfo;

      // Check if this is a DM channel so we can decrypt the content
      const serverState = stateRef.current.servers[serverId];
      const dmEntry = serverState?.dms.find(d => d.channel.id === message.channel_id);

      if (dmEntry) {
        // Decrypt asynchronously then dispatch
        const peerPk = publicKeyToString(dmEntry.participant.public_key);
        api.dmDecrypt(peerPk, message.content)
          .then((plaintext) => {
            const decryptedMsg = { ...message, content: plaintext };
            if (serverId === activeRef.current) {
              dispatch({ type: "NEW_MESSAGE", serverId, payload: decryptedMsg });
              // Reading it right now needs no pop-up; reading it from behind
              // another program does.
              if (appIsInBackground()) notifyIfWanted(serverId, decryptedMsg, plaintext);
            } else {
              dispatch({ type: "INCREMENT_UNREAD", serverId });
              notifyIfWanted(serverId, decryptedMsg, plaintext);
            }
          })
          .catch(() => {
            // Decryption failed (e.g. message from before E2EE was set up) — dispatch as-is
            if (serverId === activeRef.current) {
              dispatch({ type: "NEW_MESSAGE", serverId, payload: message });
            } else {
              dispatch({ type: "INCREMENT_UNREAD", serverId });
            }
          });
        return;
      }

      if (serverId === activeRef.current) {
        dispatch({ type: "NEW_MESSAGE", serverId, payload: message });
        if (appIsInBackground()) notifyIfWanted(serverId, message, message.content);
      } else {
        dispatch({ type: "INCREMENT_UNREAD", serverId });
        notifyIfWanted(serverId, message, message.content);
      }
    }).then(safePush);

    listen("server:message_edited", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "MESSAGE_EDITED", serverId, payload: {
        channelId: data.channel_id as number,
        messageId: data.message_id as number,
        newContent: data.new_content as string,
        editedAt: data.edited_at as number,
      }});
    }).then(safePush);

    listen("server:message_deleted", (e) => {
      const data = e.payload as MessageDeletedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({
        type: "MESSAGE_DELETED",
        serverId,
        payload: { channelId: data.channel_id, messageId: data.message_id },
      });
      // Compliant-client purge rule: server-side a delete only removes the
      // ciphertext, so end to end it means nothing unless this device also drops
      // its own decrypted copy. The TRANSLATION is a second decrypted copy,
      // keyed by message id in memory, and the row unmounts on delete — so the
      // component-level cleanup never runs and only this reaches it.
      dismissTranslation(String(data.message_id));
      void api
        .historyPurgeMessage(data.channel_id, data.message_id)
        .catch((err) => console.warn("[history] purge failed:", err));
    }).then(safePush);

    listen("server:sealed_message", (e) => {
      const data = e.payload as { server_id: string; channel_id: number; message: MessageInfoV2 };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      const message = flattenMessageInfoV2([data.message])[0];
      dispatch({ type: "ADD_OR_UPDATE_MESSAGE", serverId, payload: message });
    }).then(safePush);

    listen("server:sealed_message_edited", (e) => {
      const data = e.payload as { server_id: string; channel_id: number; message: MessageInfoV2 };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      const message = flattenMessageInfoV2([data.message])[0];
      dispatch({ type: "ADD_OR_UPDATE_MESSAGE", serverId, payload: message });
    }).then(safePush);

    listen("server:message_tombstoned", (e) => {
      const data = e.payload as { server_id: string; channel_id: number; message_id: number };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({
        type: "MESSAGE_DELETED",
        serverId,
        payload: { channelId: data.channel_id, messageId: data.message_id },
      });
      // Compliant-client purge rule: server-side a delete only removes the
      // ciphertext, so end to end it means nothing unless this device also drops
      // its own decrypted copy. The TRANSLATION is a second decrypted copy,
      // keyed by message id in memory, and the row unmounts on delete — so the
      // component-level cleanup never runs and only this reaches it.
      dismissTranslation(String(data.message_id));
      void api
        .historyPurgeMessage(data.channel_id, data.message_id)
        .catch((err) => console.warn("[history] purge failed:", err));
    }).then(safePush);

    listen("server:channel_created_v2", (e) => {
      const data = e.payload as { server_id: string; channel: ChannelInfoV2 };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      const channel = flattenChannelInfoV2([data.channel])[0];
      dispatch({ type: "CHANNEL_CREATED", serverId, payload: channel });
    }).then(safePush);

    listen("server:mls_control_event", (e) => {
      const data = e.payload as { server_id: string; channel_id: number | null; event_hash: string; payload_type: string };
      // Record for the steward (T9). No active-server filter: a commit/welcome
      // for a background server must still advance that server's ratchet.
      dispatch({
        type: "MLS_CONTROL_EVENT",
        serverId: data.server_id,
        payload: {
          channelId: data.channel_id,
          eventHash: data.event_hash,
          payloadType: data.payload_type,
        },
      });
      // A server-scoped MlsKeyPackagePublished (channel_id == null) is the
      // signal that a member just became addable. This closes the C2 race: a
      // late joiner is skipped by the membership_changed auto-add (they have no
      // package until they open the channel), so the owner retries the add here,
      // when the package actually exists. Owner-gated on the frontend (the add
      // path has no server-side owner check); the add loop is idempotent so a
      // redundant pass is a cheap no-op, never a spin.
      if (data.channel_id == null && data.payload_type === "MlsKeyPackagePublished") {
        const logServerId = stateRef.current.servers[data.server_id]?.logServerId ?? null;
        const serverState = stateRef.current.servers[data.server_id];
        const e2eeChannelIds = (serverState?.channels ?? [])
          .filter(isE2eeChannel)
          .map((c) => c.id);
        if (logServerId && e2eeChannelIds.length > 0) {
          getOwnPk().then((ownPk) => {
            if (!ownPk || ownPk !== serverState?.ownerPublicKey) return;
            for (const channelId of e2eeChannelIds) {
              api.addMembersToE2eeChannel(data.server_id, logServerId, channelId).catch(() => {});
            }
          }).catch(() => {});
        }
      }

      // Trigger the steward (T9): fetch + apply the channel's MLS control plane
      // in order. Channel-scoped events only (a server-scoped KeyPackage
      // publication carries channel_id == null and has no group to advance).
      // The steward is cursor-based + idempotent, so firing per event is cheap
      // and never spins; failures (e.g. identity still locked, no key package
      // published yet) are non-fatal and logged by the backend.
      if (data.channel_id != null) {
        const logServerId = stateRef.current.servers[data.server_id]?.logServerId ?? null;
        if (logServerId) {
          api.processMlsControlEvents(data.server_id, logServerId, data.channel_id)
            .then((result) => {
              // Surface the steward's verdict into state (T11 renders T9's
              // result). No active-server filter: a background server's ratchet
              // state is still valid and should render if it becomes active.
              dispatch({
                type: "MLS_STATE",
                serverId: data.server_id,
                payload: {
                  channelId: result.channel_id,
                  confirmed: result.confirmed,
                  outcome: result.outcome as ("advanced" | "equivocation"),
                  reason: result.reason,
                },
              });
            })
            .catch(() => {});
        }
      }
    }).then(safePush);

    listen("server:attachment_redacted", (e) => {
      const data = e.payload as { server_id: string; content_hash: string; by_moderator: boolean };
      if (data.server_id !== activeRef.current) return;
      dispatch({ type: "ATTACHMENT_REDACTED", serverId: data.server_id, payload: { contentHash: data.content_hash, byModerator: data.by_moderator } });
    }).then(safePush);

    listen("server:reaction_added", (e) => {
      const data = e.payload as ReactionAddedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      getOwnPk().then((ownPk) => {
        const isMe = ownPk != null && data.public_key === ownPk;
        dispatch({
          type: "REACTION_ADDED",
          serverId,
          payload: {
            channelId: data.channel_id,
            messageId: data.message_id,
            emoji: data.emoji,
            me: isMe,
            fileId: data.file_id,
          },
        });
      });
    }).then(safePush);

    listen("server:reaction_removed", (e) => {
      const data = e.payload as ReactionRemovedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({
        type: "REACTION_REMOVED",
        serverId,
        payload: {
          channelId: data.channel_id,
          messageId: data.message_id,
          emoji: data.emoji,
          fileId: data.file_id,
        },
      });
    }).then(safePush);

    listen("server:member_banned", (e) => {
      const data = e.payload as { server_id: string; public_key: string; reason?: string };
      window.dispatchEvent(new CustomEvent("farder:banned-list-changed", { detail: { serverId: data.server_id } }));
    }).then(safePush);

    listen("server:member_unbanned", (e) => {
      const data = e.payload as { server_id: string; public_key: string };
      window.dispatchEvent(new CustomEvent("farder:banned-list-changed", { detail: { serverId: data.server_id } }));
    }).then(safePush);

    listen("server:member_timeout_changed", (e) => {
      const data = e.payload as { server_id: string; public_key: string; until_ms: number | null; reason: string | null };
      dispatch({
        type: "MEMBER_TIMEOUT_CHANGED",
        serverId: data.server_id,
        payload: { publicKey: data.public_key, untilMs: data.until_ms, reason: data.reason },
      });
    }).then(safePush);

    listen("server:you_were_kicked", (e) => {
      const data = e.payload as { server_id: string };
      const sid = data.server_id;
      // Capture the server name before ejecting it from state.
      const serverName =
        stateRef.current.serverList.find((s) => s.id === sid)?.name ?? "the server";
      // Set the notice flag (carries server name so the dialog works after ejection).
      dispatch({ type: "YOU_WERE_KICKED", serverId: sid, serverName });
      // Immediately disconnect and eject the server from client state.
      // Voice cleanup is best-effort; failure does not block ejection.
      api.voiceLeave().catch(() => {});
      dispatch({ type: "LEAVE_VOICE_CHANNEL", serverId: sid });
      api.disconnectServer(sid).catch(() => {});
      dispatch({ type: "SERVER_REMOVED", serverId: sid });
    }).then(safePush);

    listen("server:you_were_banned", (e) => {
      const data = e.payload as { server_id: string; reason: string | null };
      const sid = data.server_id;
      // Capture the server name before ejecting it from state.
      const serverName =
        stateRef.current.serverList.find((s) => s.id === sid)?.name ?? "the server";
      // Set the notice flag (carries server name so the dialog works after ejection).
      dispatch({ type: "YOU_WERE_BANNED", serverId: sid, serverName, reason: data.reason });
      // Immediately disconnect and eject the server from client state.
      api.voiceLeave().catch(() => {});
      dispatch({ type: "LEAVE_VOICE_CHANNEL", serverId: sid });
      api.disconnectServer(sid).catch(() => {});
      dispatch({ type: "SERVER_REMOVED", serverId: sid });
    }).then(safePush);

    listen("server:audit_event_created", (e) => {
      const data = e.payload as { server_id: string; event: AuditEvent };
      // Cross-component pubsub — AuditLogTab listens for this directly via window event.
      window.dispatchEvent(new CustomEvent("farder:audit-event-created", { detail: data }));
    }).then(safePush);

    listen("server:member_joined", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current && notifPrefs?.notifyOnMemberJoin) {
        api.showNotification("Farder", `${data.display_name ?? "Someone"} joined the server`).catch(() => {});
      }
      if (serverId !== activeRef.current) return;
      // Bridge sends { public_key, display_name } as separate fields, not a MemberInfo object
      // Re-fetch the full member list to get accurate data
      api.getMembers(serverId).then(members => {
        dispatch({ type: "SET_MEMBERS", serverId, payload: members });
      }).catch((e) => console.error("[members] refresh on join failed:", e));
    }).then(safePush);

    listen("server:member_profile_updated", (e) => {
      const data = e.payload as { server_id: string };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      api.getMembers(serverId).then(members => {
        dispatch({ type: "SET_MEMBERS", serverId, payload: members });
      }).catch((err) => console.error("[members] refresh on profile update failed:", err));
    }).then(safePush);

    listen("server:membership_changed", (e) => {
      const data = e.payload as { server_id: string; public_key: string };
      const serverId = data.server_id;
      // A power may have been granted or taken away: screens that show powers re-ask.
      window.dispatchEvent(new CustomEvent("farder:capabilities-changed", { detail: { serverId } }));
      // Re-fetch my own status (I may have just been approved/denied), the member
      // list, and the pending queue — all derive from the changed log membership.
      api.getMembershipStatus(serverId).then(status => {
        dispatch({ type: "SET_MEMBERSHIP_STATUS", serverId, status: status as "member" | "pending" | "none" });
        // Becoming a member is the moment content stops being blocked. Until
        // this refresh existed, an APPROVED joiner sat with an empty channel
        // list and no categories until they restarted the app: the server had
        // denied the channel fetch while they were pending, and nothing ever
        // asked again. Status and the member list were refreshed here; the
        // channels were not.
        if (status === "member") {
          refreshServerClasses(serverId, dispatch, api);
        }
      }).catch(() => {});
      api.getMembers(serverId).then(members =>
        dispatch({ type: "SET_MEMBERS", serverId, payload: members })).catch(() => {});
      // The approval queue component refetches getPendingMembers on this event (Task 8).

      // A member who joins AFTER an E2EE channel exists is never added by the
      // creation-time add loop, so the owner auto-adds them here. NOTE: there is
      // no server-side owner gate on the add path (the fold authorizes any full
      // member holding a confirmed leaf to author an add-commit); the frontend
      // gate below is the real protection, so keep it. Gate on our own public
      // key matching the per-server `ownerPublicKey`. One pass per event, no
      // poll loop: the add loop is idempotent, so a redundant pass is a no-op.
      const serverState = stateRef.current.servers[serverId];
      const logServerId = serverState?.logServerId ?? null;
      if (!logServerId) return;
      const e2eeChannelIds = (serverState?.channels ?? [])
        .filter(isE2eeChannel)
        .map((c) => c.id);
      if (e2eeChannelIds.length === 0) return;
      getOwnPk().then((ownPk) => {
        if (!ownPk || ownPk !== serverState?.ownerPublicKey) return;
        for (const channelId of e2eeChannelIds) {
          api.addMembersToE2eeChannel(serverId, logServerId, channelId).catch(() => {});
        }
      }).catch(() => {});
    }).then(safePush);

    listen("server:permissions_changed", (e) => {
      const data = e.payload as { server_id: string };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      // A role was assigned/removed (or a role's permissions changed): refresh the
      // member list (member.role_ids) and the server info (the roles themselves) so
      // the change is reflected immediately.
      api.getMembers(serverId).then(members =>
        dispatch({ type: "SET_MEMBERS", serverId, payload: members })).catch(() => {});
      api.getServerInfoV2(serverId).then(info =>
        dispatch({ type: "SERVER_REFRESHED", serverId, payload: info })).catch(() => {});
    }).then(safePush);

    listen("server:member_presence_updated", (e) => {
      const data = e.payload as { server_id: string; public_key: string; presence: Presence | null };
      console.log("[presence] member_presence_updated", data.public_key, data.presence);
      dispatch({
        type: "UPDATE_MEMBER_PRESENCE",
        serverId: data.server_id,
        payload: { publicKey: data.public_key, presence: data.presence },
      });
    }).then(safePush);

    listen("server:member_left", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current && notifPrefs?.notifyOnMemberLeave) {
        api.showNotification("Farder", "A member left the server").catch(() => {});
      }
      if (serverId !== activeRef.current) return;
      dispatch({ type: "MEMBER_LEFT", serverId, payload: { publicKey: data.public_key as string } });
    }).then(safePush);

    // A member's data deletion was executed server-side: their messages were
    // anonymized and their files removed. Two things follow from that here.
    // Written out rather than looped over a table: `seam_audit.py` matches the
    // event name as a LITERAL in the listen() call, and a loop hides it — the
    // audit reported both of these as orphan emits, which is the check working.
    listen("server:message_pinned", (e) => {
      const data = e.payload as { server_id: string; channel_id: number; message_id: number };
      if (data.server_id !== activeRef.current) return;
      dispatch({
        type: "MESSAGE_PIN_CHANGED",
        serverId: data.server_id,
        payload: { channelId: data.channel_id, messageId: data.message_id, pinned: true },
      });
    }).then(safePush);

    listen("server:message_unpinned", (e) => {
      const data = e.payload as { server_id: string; channel_id: number; message_id: number };
      if (data.server_id !== activeRef.current) return;
      dispatch({
        type: "MESSAGE_PIN_CHANGED",
        serverId: data.server_id,
        payload: { channelId: data.channel_id, messageId: data.message_id, pinned: false },
      });
    }).then(safePush);

    // A reply typed into the notification pop-up. The pop-up only ASKS; this checks
    // the request against what really exists and sends it the way the message box
    // does (a DM is encrypted first). The answer goes back so the pop-up can say
    // "Sent" or show the error.
    let lastReplyAt = 0;
    listen(REPLY_EVENT, async (e) => {
      const req = e.payload as ReplyRequest;
      const respond = (r: Omit<ReplyResult, "id">) =>
        emit(REPLY_RESULT_EVENT, { id: typeof req?.id === "string" ? req.id : "", ...r }).catch(() => {});
      const now = Date.now();
      if (now - lastReplyAt < MIN_REPLY_GAP_MS) { void respond({ ok: false, error: "Slow down." }); return; }
      lastReplyAt = now;
      const server = typeof req?.serverId === "string" ? stateRef.current.servers[req.serverId] : undefined;
      const view: ReplyServerView | undefined = server && {
        channels: server.channels.map((c) => ({ id: c.id, class: c.class })),
        dms: server.dms.map((d) => ({ channel: { id: d.channel.id }, peerKey: publicKeyToString(d.participant.public_key) })),
        logServerId: server.logServerId,
      };
      const planned = planReply(req, view);
      if (!planned.ok) { void respond({ ok: false, error: planned.error }); return; }
      // Your own "speak as" setting applies here exactly as it does in the message box.
      const g = getGarble();
      const text = g.enabled ? garbleText(planned.text, makeTransform(g.mode, g.phrase)) : planned.text;
      try {
        await executeReply(planned.plan, req, text, {
          sendMessage: (s, c, t) => api.sendMessage(s, c, t),
          submitEvent: (s, l, c, t, r, caps) => api.submitEvent(s, l, c, t, r, caps),
          dmEncrypt: (peer, t) => api.dmEncrypt(peer, t),
        });
        void respond({ ok: true });
      } catch (err) {
        void respond({ ok: false, error: String(err) });
      }
    }).then(safePush);

    // --- the edge strip ---------------------------------------------------
    // The strip window announces itself when its page loads: send it the state.
    listen(STRIP_READY_EVENT, () => publishStrip()).then(safePush);
    // A conversation was answered from the strip: it is dealt with.
    listen("strip:replied", (e) => {
      const t = e.payload as { server_id?: unknown; channel_id?: unknown } | null;
      if (t && typeof t.server_id === "string" && typeof t.channel_id === "number") {
        strip.resolve(t.server_id, t.channel_id);
        publishStrip();
      }
    }).then(safePush);
    // A chip or the preview was clicked: show that conversation. The payload comes
    // from another window, so it is checked against what actually exists.
    listen(STRIP_OPEN_EVENT, (e) => {
      const o = parseOpen(e.payload);
      if (!o) return;
      const server = stateRef.current.servers[o.serverId];
      if (!server) return;
      dispatch({ type: "SET_ACTIVE_SERVER", serverId: o.serverId });
      if (o.channelId === null) return;
      if (server.dms.some((d) => d.channel.id === o.channelId)) {
        dispatch({ type: "OPEN_DM_PANEL", serverId: o.serverId, payload: o.channelId });
      } else if (server.channels.some((c) => c.id === o.channelId)) {
        dispatch({ type: "SELECT_CHANNEL", serverId: o.serverId, payload: o.channelId });
      }
    }).then(safePush);
    // Settings changed (turned on or off).
    listen("strip:settings-changed", (e) => {
      const on = (e.payload as { enabled?: unknown } | null)?.enabled === true;
      stripEnabled = on;
      if (!on) strip.clear();
    }).then(safePush);
    // Looking at Farder means the unread chips have been seen.
    const onFocus = () => { if (stripEnabled) { strip.clear(); publishStrip(); } };
    window.addEventListener("focus", onFocus);
    safePush(() => window.removeEventListener("focus", onFocus));

    // A garble rule or consent changed somewhere on this server: tell whoever is
    // showing garble state (message input banner, settings, member menu) to re-ask.
    listen("server:garble_changed", (e) => {
      const data = e.payload as { server_id: string };
      window.dispatchEvent(new CustomEvent("farder:garble-changed", { detail: { serverId: data.server_id } }));
    }).then(safePush);

    listen("server:member_data_deleted", (e) => {
      const data = e.payload as { server_id: string; public_key: string; public_key_bytes: number[] };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      // 1. The roster. Without this the deleted member sits in the member list
      //    until the next reconnect.
      dispatch({ type: "MEMBER_LEFT", serverId, payload: { publicKey: data.public_key } });
      // 2. The compliant-client purge. For an encrypted channel the server just
      //    anonymized ciphertext; the readable copy is the one on this device,
      //    so the deletion means nothing end to end unless it goes too.
      void api
        .historyPurgeAuthor(data.public_key_bytes)
        .catch((err) => console.warn("[history] purge author failed:", err));
      // Translations are keyed by message id alone — there is no author index to
      // purge by, so the fail-closed answer is to forget all of them. Cheap to
      // redo, impossible to retract.
      clearTranslations();
    }).then(safePush);

    listen("server:channel_created", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CHANNEL_CREATED", serverId, payload: data.channel as ChannelInfo });
    }).then(safePush);

    listen("server:channel_deleted", (e) => {
      const data = e.payload as ChannelDeletedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CHANNEL_DELETED", serverId, payload: { channelId: data.channel_id } });
    }).then(safePush);

    listen("server:category_created", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CATEGORY_CREATED", serverId, payload: data.category as CategoryInfo });
    }).then(safePush);

    listen("server:category_deleted", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CATEGORY_DELETED", serverId, payload: { categoryId: data.category_id as number } });
    }).then(safePush);

    listen("server:category_updated", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CATEGORY_UPDATED", serverId, payload: data.category as CategoryInfo });
    }).then(safePush);

    listen("server:channel_updated", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CHANNEL_UPDATED", serverId, payload: data.channel as ChannelInfo });
    }).then(safePush);

    listen("server:disconnected", (e) => {
      const data = e.payload as any;
      dispatch({ type: "CONNECTION_LOST", serverId: data.server_id as string });
    }).then(safePush);

    listen("server:dm_created", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "DM_CREATED", serverId, payload: { channel: data.channel, participant: data.participant } });
    }).then(safePush);

    listen("server:role_created", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "ROLE_CREATED", serverId, payload: data.role as RoleInfo });
    }).then(safePush);

    listen("server:role_deleted", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "ROLE_DELETED", serverId, payload: { roleId: data.role_id as number } });
    }).then(safePush);

    listen("server:role_updated", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "ROLE_UPDATED", serverId, payload: data.role as RoleInfo });
    }).then(safePush);

    listen("server:typing", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      const channelId = data.channel_id as number;
      const publicKey = data.public_key as string;
      dispatch({ type: "TYPING_STARTED", serverId, payload: { channelId, publicKey, displayName: publicKey } });
      setTimeout(() => {
        dispatch({ type: "TYPING_EXPIRED", serverId, payload: { channelId, publicKey } });
      }, 8000);
    }).then(safePush);

    // Widget events — dropped for background servers like other message-adjacent
    // events; widgets re-hydrate via getPoll/getGiveaway on next mount.
    listen("server:poll_updated", (e) => {
      const data = e.payload as { server_id: string; poll: PollInfo };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "POLL_UPDATED", serverId, payload: data.poll });
    }).then(safePush);

    listen("server:giveaway_updated", (e) => {
      const data = e.payload as { server_id: string; giveaway: GiveawayInfo };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "GIVEAWAY_UPDATED", serverId, payload: data.giveaway });
    }).then(safePush);

    listen("server:event_updated", (e) => {
      const data = e.payload as { server_id: string; event: EventInfo };
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "EVENT_UPDATED", serverId, payload: data.event });
    }).then(safePush);

    // Voice events — dispatched for ALL servers so voice activity is visible across servers
    listen("server:voice_joined", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      dispatch({ type: "VOICE_JOINED", serverId, payload: {
        channelId: data.channel_id as number,
        publicKey: data.public_key as string,
        displayName: data.display_name as string,
      }});
      // The activity log's live refresh. This event already crossed the wire
      // for the voice roster, so the log view rides on it rather than the
      // server broadcasting a second copy of every join to every admin.
      window.dispatchEvent(new CustomEvent("farder:voice-activity"));
    }).then(safePush);

    listen("server:voice_left", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      dispatch({ type: "VOICE_LEFT", serverId, payload: {
        channelId: data.channel_id as number,
        publicKey: data.public_key as string,
      }});
      window.dispatchEvent(new CustomEvent("farder:voice-activity"));
    }).then(safePush);

    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, [dispatch]);
}
