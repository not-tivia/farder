import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useApp } from "../context/ServerContext";
import type { MessageInfo, ChannelInfo, CategoryInfo, RoleInfo, Presence, PollInfo, GiveawayInfo, EventInfo, MessageInfoV2, ChannelInfoV2 } from "../lib/types";
import { publicKeyToString, flattenMessageInfoV2, flattenChannelInfoV2, isE2eeChannel } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import { refreshServerClasses } from "../lib/refreshServerClasses";
import type { NotificationPrefs, AuditEvent } from "../lib/tauri-bridge";

// Module-level cache for notification prefs and own public key
let notifPrefs: NotificationPrefs | null = null;
api.getNotificationPrefs().then(p => { notifPrefs = p; }).catch(() => {});
// Own public key, fetched LAZILY: at module load the identity is still
// PIN-locked, so an eager getPublicKey() fails and would leave this null for
// the whole session (making every own-reaction event look like someone
// else's — the stacking-reactions bug). A failed fetch retries next call.
let cachedOwnPk: string | null = null;
let ownPkPromise: Promise<string | null> | null = null;
function getOwnPk(): Promise<string | null> {
  if (cachedOwnPk != null) return Promise.resolve(cachedOwnPk);
  if (!ownPkPromise) {
    ownPkPromise = api
      .getPublicKey()
      .then((pk) => {
        cachedOwnPk = pk;
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
            } else {
              dispatch({ type: "INCREMENT_UNREAD", serverId });
              if (notifPrefs && shouldNotify(serverId, decryptedMsg, notifPrefs)) {
                api.showNotification("Farder", plaintext.slice(0, 120)).catch(() => {});
              } else if (!notifPrefs) {
                api.showNotification("Farder", plaintext.slice(0, 120)).catch(() => {});
              }
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
      } else {
        dispatch({ type: "INCREMENT_UNREAD", serverId });

        if (message.content && notifPrefs && shouldNotify(serverId, message, notifPrefs)) {
          api.showNotification("Farder", message.content.slice(0, 120)).catch(() => {});
        } else if (message.content && !notifPrefs) {
          // Prefs not yet loaded — fall back to always notify
          api.showNotification("Farder", message.content.slice(0, 120)).catch(() => {});
        }
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
      // its own decrypted copy.
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
      // its own decrypted copy.
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
    }).then(safePush);

    listen("server:voice_left", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      dispatch({ type: "VOICE_LEFT", serverId, payload: {
        channelId: data.channel_id as number,
        publicKey: data.public_key as string,
      }});
    }).then(safePush);

    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, [dispatch]);
}
