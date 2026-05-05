import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useApp } from "../context/ServerContext";
import type { MessageInfo, ChannelInfo, CategoryInfo, RoleInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import type { NotificationPrefs } from "../lib/tauri-bridge";

// Module-level cache for notification prefs and own public key
let notifPrefs: NotificationPrefs | null = null;
api.getNotificationPrefs().then(p => { notifPrefs = p; }).catch(() => {});
let cachedOwnPk: string | null = null;
api.getPublicKey().then(pk => { cachedOwnPk = pk; }).catch(() => {});

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
    }).then(safePush);

    listen("server:reaction_added", (e) => {
      const data = e.payload as ReactionAddedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      const isMe = cachedOwnPk != null && data.public_key === cachedOwnPk;
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
      }).catch(() => {});
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
