import React, { createContext, useContext, useReducer, ReactNode } from "react";
import type { ChannelInfo, CategoryInfo, RoleInfo, MemberInfo, MessageInfo, ConnectResult, DmEntry, ServerListEntry, Presence, PollInfo, GiveawayInfo, EventInfo, ServerInfoV2, MlsControlEventInfo, MlsChannelStateInfo, SealedDecryptEntry } from "../lib/types";
import { publicKeyToString, flattenChannelInfoV2 } from "../lib/types";
import type { NoticeRow } from "../lib/tauri-bridge";

export interface PerServerState {
  serverName: string;
  connected: boolean;
  connectionLost: boolean;
  channels: ChannelInfo[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
  members: MemberInfo[];
  currentChannelId: number | null;
  messages: Record<number, MessageInfo[]>;
  threadChannelId: number | null;
  readState: Record<number, number>;
  dms: DmEntry[];
  dmPanelChannelId: number | null;
  typingUsers: Record<number, { publicKey: string; displayName: string; expiresAt: number }[]>;
  voiceStates: Record<number, { publicKey: string; displayName: string }[]>;
  currentVoiceChannelId: number | null;
  ownerPublicKey: string | null;
  relayed: boolean;
  logServerId: string | null;
  highlightMessageId: number | null;
  membershipStatus: "member" | "pending" | "none";
  /** Poll widget state keyed by poll id (per-server, so ids never collide across servers). */
  polls: Record<number, { poll: PollInfo; myVote: number | null }>;
  /** Giveaway widget state keyed by giveaway id. */
  giveaways: Record<number, { giveaway: GiveawayInfo; myEntered: boolean }>;
  /** Event widget state keyed by event id. `myRsvp` is self-only ("going" |
   *  "maybe" | "no" | null) and never arrives in a broadcast. */
  events: Record<number, { event: EventInfo; myRsvp: string | null }>;
  /** The viewed channel's open-widget id lists for the active-widgets bar
   *  (ids only — the infos live in `polls`/`giveaways`/`events`, one source of
   *  truth with the widgets). Replaced whole by `ACTIVE_WIDGETS` on channel
   *  switch/reconnect; maintained live by `POLL_UPDATED`/`GIVEAWAY_UPDATED`/
   *  `EVENT_UPDATED`. `null` until the first fetch. */
  activeWidgets: { channelId: number; polls: number[]; giveaways: number[]; events: number[] } | null;
  /** Pending MLS control events (KeyPackage / Commit / Welcome /
   *  LeafConfirmed / GroupReset) received via `server:mls_control_event`.
   *  The steward (T9, 4b-2) drains these - T4 only records them, deduped by
   *  `eventHash`. */
  mlsControlEvents: MlsControlEventInfo[];
  /** Per-channel MLS group state derived from the T9 steward result (keyed by
   *  channel id). Absent entry = unknown; the UI never gates a channel on it.
   *  `outcome === "equivocation"` is the terminal poisoned state (F4). */
  mlsStates: Record<number, MlsChannelStateInfo>;
  /** Per-message decrypt results for sealed rows (D2/D4): keyed by message id.
   *  A sealed row is decrypted exactly once (the ratchet is consumed on open);
   *  the result is cached here so a re-render never re-opens the ciphertext.
   *  `decrypted` holds the plaintext; `undecryptable` is the distinct
   *  "couldn't decrypt" marker. Absent = not yet decrypted (T5 placeholder). */
  sealedDecrypts: Record<number, SealedDecryptEntry>;
  /** Channels whose locally stored history has been loaded into
   *  `sealedDecrypts`. Load-bearing, not bookkeeping: `useSealedDecrypt` must
   *  NOT open a ciphertext before this is true for its channel, because a
   *  message we already hold locally has had its ratchet key consumed — the
   *  open would fail and cache "couldn't decrypt" over restored history. */
  historyHydrated: Record<number, boolean>;
  /** Transparency notices per channel (sub-5b G1), oldest-first. Non-dismissible
   *  by design: a leaf-set change is a security-relevant fact, not a toast. */
  notices: Record<number, NoticeRow[]>;
}

export interface AppState {
  hasIdentity: boolean;
  activeServerId: string | null;
  serverList: ServerListEntry[];
  servers: Record<string, PerServerState>;
  kickedBanned: { kind: "kick" | "ban"; serverId: string; serverName: string; reason: string | null } | null;
  joinConfirmLink: string | null;
}

// Keep old ServerState as alias for backward compat
export type ServerState = PerServerState;

const initialPerServerState: PerServerState = {
  serverName: "",
  connected: true,
  connectionLost: false,
  channels: [],
  categories: [],
  roles: [],
  members: [],
  currentChannelId: null,
  messages: {},
  threadChannelId: null,
  readState: {},
  dms: [],
  dmPanelChannelId: null,
  typingUsers: {},
  voiceStates: {},
  currentVoiceChannelId: null,
  ownerPublicKey: null,
  relayed: false,
  logServerId: null,
  highlightMessageId: null,
  membershipStatus: "member",
  polls: {},
  giveaways: {},
  events: {},
  activeWidgets: null,
  mlsControlEvents: [],
  mlsStates: {},
  sealedDecrypts: {},
  historyHydrated: {},
  notices: {},
};

/** Combined chip cap for the active-widgets bar — mirrors the server's
 *  `ListActiveWidgets` 20-combined truncation. */
const ACTIVE_WIDGETS_CAP = 20;

const initialAppState: AppState = {
  hasIdentity: false,
  activeServerId: null,
  serverList: [],
  servers: {},
  kickedBanned: null,
  joinConfirmLink: null,
};

export type AppAction =
  // App-level actions
  | { type: "SET_IDENTITY" }
  | { type: "SERVER_ADDED"; serverId: string; payload: ConnectResult }
  | { type: "SERVER_REMOVED"; serverId: string }
  | { type: "SET_ACTIVE_SERVER"; serverId: string }
  | { type: "UPDATE_SERVER_LIST"; payload: ServerListEntry[] }
  | { type: "INCREMENT_UNREAD"; serverId: string }
  | { type: "CLEAR_UNREAD"; serverId: string }
  // Per-server actions (all require serverId)
  | { type: "CONNECTED"; serverId: string; payload: ServerInfoV2 }
  | { type: "SERVER_REFRESHED"; serverId: string; payload: ServerInfoV2 }
  | { type: "DISCONNECTED"; serverId: string }
  | { type: "CONNECTION_LOST"; serverId: string }
  | { type: "RECONNECTED"; serverId: string }
  | { type: "SET_MEMBERS"; serverId: string; payload: MemberInfo[] }
  | { type: "SELECT_CHANNEL"; serverId: string; payload: number }
  | { type: "HIGHLIGHT_MESSAGE"; serverId: string; payload: { messageId: number | null } }
  | { type: "SET_MESSAGES"; serverId: string; payload: { channelId: number; messages: MessageInfo[] } }
  | { type: "PREPEND_MESSAGES"; serverId: string; payload: { channelId: number; messages: MessageInfo[] } }
  | { type: "NEW_MESSAGE"; serverId: string; payload: MessageInfo }
  | { type: "MESSAGE_EDITED"; serverId: string; payload: { channelId: number; messageId: number; newContent: string; editedAt: number } }
  | { type: "MESSAGE_DELETED"; serverId: string; payload: { channelId: number; messageId: number } }
  | { type: "ATTACHMENT_REDACTED"; serverId: string; payload: { contentHash: string; byModerator: boolean } }
  | { type: "REACTION_ADDED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string; me: boolean; fileId?: number } }
  | { type: "REACTION_REMOVED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string; fileId?: number } }
  | { type: "MEMBER_JOINED"; serverId: string; payload: MemberInfo }
  | { type: "MEMBER_LEFT"; serverId: string; payload: { publicKey: string } }
  | { type: "CHANNEL_CREATED"; serverId: string; payload: ChannelInfo }
  | { type: "CHANNEL_DELETED"; serverId: string; payload: { channelId: number } }
  | { type: "CATEGORY_CREATED"; serverId: string; payload: CategoryInfo }
  | { type: "CATEGORY_DELETED"; serverId: string; payload: { categoryId: number } }
  | { type: "CATEGORY_UPDATED"; serverId: string; payload: CategoryInfo }
  | { type: "CHANNEL_UPDATED"; serverId: string; payload: ChannelInfo }
  | { type: "VIEW_THREAD"; serverId: string; payload: number | null }
  | { type: "MARK_READ"; serverId: string; payload: { channelId: number; lastMessageId: number } }
  | { type: "SET_DMS"; serverId: string; payload: DmEntry[] }
  | { type: "DM_CREATED"; serverId: string; payload: { channel: ChannelInfo; participant: MemberInfo } }
  | { type: "OPEN_DM_PANEL"; serverId: string; payload: number }
  | { type: "CLOSE_DM_PANEL"; serverId: string }
  | { type: "TYPING_STARTED"; serverId: string; payload: { channelId: number; publicKey: string; displayName: string } }
  | { type: "TYPING_EXPIRED"; serverId: string; payload: { channelId: number; publicKey: string } }
  | { type: "ROLE_CREATED"; serverId: string; payload: RoleInfo }
  | { type: "ROLE_DELETED"; serverId: string; payload: { roleId: number } }
  | { type: "ROLE_UPDATED"; serverId: string; payload: RoleInfo }
  | { type: "VOICE_JOINED"; serverId: string; payload: { channelId: number; publicKey: string; displayName: string } }
  | { type: "VOICE_LEFT"; serverId: string; payload: { channelId: number; publicKey: string } }
  | { type: "SET_VOICE_STATE"; serverId: string; payload: { channelId: number; participants: { publicKey: string; displayName: string }[] } }
  | { type: "JOIN_VOICE_CHANNEL"; serverId: string; payload: number }
  | { type: "LEAVE_VOICE_CHANNEL"; serverId: string }
  | { type: "MEMBER_TIMEOUT_CHANGED"; serverId: string; payload: { publicKey: string; untilMs: number | null; reason: string | null } }
  | { type: "UPDATE_MEMBER_PRESENCE"; serverId: string; payload: { publicKey: string; presence: Presence | null } }
  | { type: "YOU_WERE_KICKED"; serverId: string; serverName: string }
  | { type: "YOU_WERE_BANNED"; serverId: string; serverName: string; reason: string | null }
  | { type: "CLEAR_KICKED_BANNED" }
  | { type: "OPEN_JOIN_CONFIRM"; link: string }
  | { type: "CLOSE_JOIN_CONFIRM" }
  | { type: "SET_MEMBERSHIP_STATUS"; serverId: string; status: "member" | "pending" | "none" }
  | { type: "POLL_UPDATED"; serverId: string; payload: PollInfo }
  | { type: "POLL_STATE"; serverId: string; payload: { poll: PollInfo; myVote: number | null } }
  | { type: "POLL_MY_VOTE"; serverId: string; payload: { pollId: number; myVote: number | null } }
  | { type: "GIVEAWAY_UPDATED"; serverId: string; payload: GiveawayInfo }
  | { type: "GIVEAWAY_STATE"; serverId: string; payload: { giveaway: GiveawayInfo; myEntered: boolean } }
  | { type: "GIVEAWAY_MY_ENTERED"; serverId: string; payload: { giveawayId: number; myEntered: boolean } }
  | { type: "EVENT_UPDATED"; serverId: string; payload: EventInfo }
  | { type: "EVENT_STATE"; serverId: string; payload: { event: EventInfo; myRsvp: string | null } }
  | { type: "EVENT_MY_RSVP"; serverId: string; payload: { eventId: number; myRsvp: string | null } }
  | { type: "ACTIVE_WIDGETS"; serverId: string; payload: { channelId: number; polls: PollInfo[]; giveaways: GiveawayInfo[]; events: EventInfo[] } }
  | { type: "ADD_OR_UPDATE_MESSAGE"; serverId: string; payload: MessageInfo }
  | { type: "MLS_CONTROL_EVENT"; serverId: string; payload: MlsControlEventInfo }
  | { type: "SEALED_DECRYPTED"; serverId: string; payload: { messageId: number; eventHash: string | null; content: string } }
  | { type: "SEALED_UNDECRYPTABLE"; serverId: string; payload: { messageId: number; eventHash: string | null; reason: string } }
  | { type: "MLS_STATE"; serverId: string; payload: { channelId: number; confirmed: boolean; outcome: "advanced" | "equivocation"; reason: string | null } }
  | { type: "HISTORY_HYDRATED"; serverId: string; payload: { channelId: number } }
  | { type: "SET_NOTICES"; serverId: string; payload: { channelId: number; notices: NoticeRow[] } };

// Keep old ServerAction as alias
export type ServerAction = AppAction;

function perServerReducer(state: PerServerState, action: AppAction): PerServerState {
  switch (action.type) {
    case "CONNECTED":
    case "SERVER_REFRESHED":
      return {
        ...state,
        connected: true,
        connectionLost: false,
        serverName: action.payload.server_name,
        channels: flattenChannelInfoV2(action.payload.channels),
        categories: action.payload.categories,
        roles: action.payload.roles,
        ownerPublicKey: action.payload.owner_public_key
          ? publicKeyToString(action.payload.owner_public_key)
          : null,
        logServerId: action.payload.server_id ?? null,
      };
    case "DISCONNECTED":
      return { ...initialPerServerState, connected: false };
    case "CONNECTION_LOST":
      return { ...state, connected: false, connectionLost: true };
    case "RECONNECTED":
      return { ...state, connected: true, connectionLost: false };
    case "SET_MEMBERS":
      return { ...state, members: action.payload };
    case "MEMBER_TIMEOUT_CHANGED": {
      const members = state.members.map((m) =>
        publicKeyToString(m.public_key) === action.payload.publicKey
          ? { ...m, timeout_until: action.payload.untilMs, timeout_reason: action.payload.reason }
          : m
      );
      return { ...state, members };
    }
    case "UPDATE_MEMBER_PRESENCE":
      return {
        ...state,
        members: state.members.map((m) =>
          publicKeyToString(m.public_key) === action.payload.publicKey
            ? { ...m, presence: action.payload.presence }
            : m,
        ),
      };
    case "SELECT_CHANNEL": {
      const chMsgs = state.messages[action.payload] ?? [];
      const latestId = chMsgs.length > 0 ? Math.max(...chMsgs.map((m) => m.id)) : 0;
      const newReadState = latestId > 0
        ? { ...state.readState, [action.payload]: latestId }
        : state.readState;
      return { ...state, currentChannelId: action.payload, threadChannelId: null, readState: newReadState };
    }
    case "HIGHLIGHT_MESSAGE":
      return { ...state, highlightMessageId: action.payload.messageId };
    case "SET_MEMBERSHIP_STATUS":
      return { ...state, membershipStatus: action.status };
    case "SET_MESSAGES":
      return {
        ...state,
        messages: { ...state.messages, [action.payload.channelId]: action.payload.messages },
      };
    case "PREPEND_MESSAGES": {
      const existing = state.messages[action.payload.channelId] ?? [];
      return {
        ...state,
        messages: {
          ...state.messages,
          [action.payload.channelId]: [...action.payload.messages, ...existing],
        },
      };
    }
    case "NEW_MESSAGE": {
      const channelId = action.payload.channel_id;
      const existing = state.messages[channelId] ?? [];
      if (existing.some((m) => m.id === action.payload.id)) return state;
      return {
        ...state,
        messages: { ...state.messages, [channelId]: [...existing, action.payload] },
      };
    }
    case "ADD_OR_UPDATE_MESSAGE": {
      // Whole-row upsert (sealed rows replace, never merge): a sealed edit
      // carries the entire row - new ciphertext, new event_hash - so the
      // v1 MESSAGE_EDITED content/edited_at patch cannot express it.
      const channelId = action.payload.channel_id;
      const existing = state.messages[channelId] ?? [];
      const idx = existing.findIndex((m) => m.id === action.payload.id);
      if (idx === -1) {
        return { ...state, messages: { ...state.messages, [channelId]: [...existing, action.payload] } };
      }
      const next = existing.slice();
      next[idx] = action.payload;
      return { ...state, messages: { ...state.messages, [channelId]: next } };
    }
    case "MESSAGE_EDITED": {
      const { channelId, messageId, newContent, editedAt } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => m.id === messageId ? { ...m, content: newContent, edited_at: editedAt } : m),
        },
      };
    }
    case "MESSAGE_DELETED": {
      const { channelId, messageId } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      return {
        ...state,
        messages: { ...state.messages, [channelId]: msgs.filter((m) => m.id !== messageId) },
      };
    }
    case "ATTACHMENT_REDACTED": {
      const { contentHash, byModerator } = action.payload;
      const messages: typeof state.messages = {};
      for (const [chId, msgs] of Object.entries(state.messages)) {
        messages[Number(chId)] = msgs.map((m) => {
          if (!m.attachments?.some((a) => a.content_hash === contentHash)) return m;
          return { ...m, attachments: m.attachments.map((a) =>
            a.content_hash === contentHash
              ? { ...a, redacted_by_moderator: byModerator }
              : a) };
        });
      }
      return { ...state, messages };
    }
    case "REACTION_ADDED": {
      const { channelId, messageId, emoji, me, fileId } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      const matches = (r: { emoji: string; file_id?: number }) =>
        r.emoji === emoji && (r.file_id ?? null) === (fileId ?? null);
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => {
            if (m.id !== messageId) return m;
            const existing = m.reactions.find(matches);
            if (existing) {
              if (me && existing.me) return m;
              const reactions = m.reactions.map((r) =>
                matches(r) ? { ...r, count: r.count + 1, me: me || r.me } : r,
              );
              return { ...m, reactions };
            }
            return { ...m, reactions: [...m.reactions, { emoji, count: 1, me, file_id: fileId }] };
          }),
        },
      };
    }
    case "REACTION_REMOVED": {
      const { channelId, messageId, emoji, fileId } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      const matches = (r: { emoji: string; file_id?: number }) =>
        r.emoji === emoji && (r.file_id ?? null) === (fileId ?? null);
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => {
            if (m.id !== messageId) return m;
            const reactions = m.reactions
              .map((r) => (matches(r) ? { ...r, count: r.count - 1 } : r))
              .filter((r) => r.count > 0);
            return { ...m, reactions };
          }),
        },
      };
    }
    case "MEMBER_JOINED":
      return { ...state, members: [...state.members, action.payload] };
    case "MEMBER_LEFT": {
      const leftPk = action.payload.publicKey;
      return {
        ...state,
        members: state.members.filter(
          (m) => publicKeyToString(m.public_key) !== leftPk,
        ),
      };
    }
    case "MLS_CONTROL_EVENT": {
      // Record-only: the steward (T9) drains this queue and processes each
      // event via fetch_mls_control. Dedupe by event_hash so a replayed
      // broadcast is not processed twice.
      if (state.mlsControlEvents.some((e) => e.eventHash === action.payload.eventHash)) return state;
      return { ...state, mlsControlEvents: [...state.mlsControlEvents, action.payload] };
    }
    case "SET_NOTICES":
      return {
        ...state,
        notices: { ...state.notices, [action.payload.channelId]: action.payload.notices },
      };
    case "HISTORY_HYDRATED":
      return {
        ...state,
        historyHydrated: { ...state.historyHydrated, [action.payload.channelId]: true },
      };
    case "SEALED_DECRYPTED":
      return {
        ...state,
        sealedDecrypts: {
          ...state.sealedDecrypts,
          [action.payload.messageId]: {
            kind: "decrypted",
            content: action.payload.content,
            eventHash: action.payload.eventHash,
          },
        },
      };
    case "SEALED_UNDECRYPTABLE":
      return {
        ...state,
        sealedDecrypts: {
          ...state.sealedDecrypts,
          [action.payload.messageId]: {
            kind: "undecryptable",
            reason: action.payload.reason,
            eventHash: action.payload.eventHash,
          },
        },
      };
    case "MLS_STATE":
      return {
        ...state,
        mlsStates: {
          ...state.mlsStates,
          [action.payload.channelId]: {
            confirmed: action.payload.confirmed,
            outcome: action.payload.outcome,
            reason: action.payload.reason,
          },
        },
      };
    case "CHANNEL_CREATED":
      if (state.channels.some(c => c.id === action.payload.id)) return state;
      return { ...state, channels: [...state.channels, { ...action.payload, class: action.payload.class ?? "Plaintext" }] };
    case "CHANNEL_DELETED": {
      const { channelId } = action.payload;
      // Prune the stale voice roster for the deleted channel, and exit voice if
      // it was the channel we were in.
      const { [channelId]: _removed, ...voiceStates } = state.voiceStates;
      return {
        ...state,
        channels: state.channels.filter((c) => c.id !== channelId),
        voiceStates,
        currentVoiceChannelId:
          state.currentVoiceChannelId === channelId ? null : state.currentVoiceChannelId,
      };
    }
    case "CATEGORY_CREATED":
      return { ...state, categories: [...state.categories, action.payload] };
    case "CATEGORY_DELETED":
      return { ...state, categories: state.categories.filter((c) => c.id !== action.payload.categoryId) };
    case "CATEGORY_UPDATED":
      return { ...state, categories: state.categories.map((c) => c.id === action.payload.id ? action.payload : c) };
    case "CHANNEL_UPDATED":
      return { ...state, channels: state.channels.map((c) => c.id === action.payload.id ? { ...action.payload, class: c.class ?? "Plaintext" } : c) };
    case "VIEW_THREAD":
      return { ...state, threadChannelId: action.payload };
    case "MARK_READ": {
      const { channelId, lastMessageId } = action.payload;
      return { ...state, readState: { ...state.readState, [channelId]: lastMessageId } };
    }
    case "SET_DMS":
      return { ...state, dms: action.payload };
    case "DM_CREATED": {
      const { channel, participant } = action.payload;
      const newEntry: DmEntry = { channel, participant, last_message: null };
      const exists = state.dms.some((d) => d.channel.id === channel.id);
      if (exists) return state;
      return { ...state, dms: [...state.dms, newEntry] };
    }
    case "OPEN_DM_PANEL":
      return { ...state, dmPanelChannelId: action.payload };
    case "CLOSE_DM_PANEL":
      return { ...state, dmPanelChannelId: null };
    case "TYPING_STARTED": {
      const { channelId, publicKey, displayName } = action.payload;
      const existing = state.typingUsers[channelId] ?? [];
      const filtered = existing.filter(t => t.publicKey !== publicKey);
      const updated = [...filtered, { publicKey, displayName, expiresAt: Date.now() + 8000 }];
      return { ...state, typingUsers: { ...state.typingUsers, [channelId]: updated } };
    }
    case "TYPING_EXPIRED": {
      const { channelId, publicKey } = action.payload;
      const existing = state.typingUsers[channelId] ?? [];
      const filtered = existing.filter(t => t.publicKey !== publicKey);
      return { ...state, typingUsers: { ...state.typingUsers, [channelId]: filtered } };
    }
    case "ROLE_CREATED":
      return { ...state, roles: [...state.roles, action.payload] };
    case "ROLE_DELETED":
      return { ...state, roles: state.roles.filter(r => r.id !== action.payload.roleId) };
    case "ROLE_UPDATED": {
      const updated = action.payload;
      const exists = state.roles.some(r => r.id === updated.id);
      return {
        ...state,
        roles: exists
          ? state.roles.map(r => r.id === updated.id ? updated : r)
          : [...state.roles, updated],
      };
    }
    case "VOICE_JOINED": {
      const { channelId, publicKey, displayName } = action.payload;
      const existing = state.voiceStates[channelId] ?? [];
      if (existing.some(v => v.publicKey === publicKey)) return state;
      return { ...state, voiceStates: { ...state.voiceStates, [channelId]: [...existing, { publicKey, displayName }] } };
    }
    case "VOICE_LEFT": {
      const { channelId, publicKey } = action.payload;
      const existing = state.voiceStates[channelId] ?? [];
      return { ...state, voiceStates: { ...state.voiceStates, [channelId]: existing.filter(v => v.publicKey !== publicKey) } };
    }
    case "SET_VOICE_STATE":
      return { ...state, voiceStates: { ...state.voiceStates, [action.payload.channelId]: action.payload.participants } };
    case "JOIN_VOICE_CHANNEL":
      return { ...state, currentVoiceChannelId: action.payload };
    case "LEAVE_VOICE_CHANNEL":
      return { ...state, currentVoiceChannelId: null };
    case "POLL_UPDATED": {
      // Broadcast events carry shared state only — preserve my existing vote.
      const poll = action.payload;
      const myVote = state.polls[poll.id]?.myVote ?? null;
      // Maintain the active-widgets bar for the viewed channel: a poll created
      // live appends its chip (no refetch); a closed one drops it. 20-cap kept.
      let activeWidgets = state.activeWidgets;
      if (activeWidgets && poll.channel_id === activeWidgets.channelId) {
        if (poll.closed) {
          if (activeWidgets.polls.includes(poll.id)) {
            activeWidgets = { ...activeWidgets, polls: activeWidgets.polls.filter((id) => id !== poll.id) };
          }
        } else if (
          !activeWidgets.polls.includes(poll.id) &&
          activeWidgets.polls.length + activeWidgets.giveaways.length + activeWidgets.events.length < ACTIVE_WIDGETS_CAP
        ) {
          activeWidgets = { ...activeWidgets, polls: [...activeWidgets.polls, poll.id] };
        }
      }
      return { ...state, activeWidgets, polls: { ...state.polls, [poll.id]: { poll, myVote } } };
    }
    case "POLL_STATE": {
      const { poll, myVote } = action.payload;
      return { ...state, polls: { ...state.polls, [poll.id]: { poll, myVote } } };
    }
    case "POLL_MY_VOTE": {
      const { pollId, myVote } = action.payload;
      const existing = state.polls[pollId];
      if (!existing) return state;
      return { ...state, polls: { ...state.polls, [pollId]: { ...existing, myVote } } };
    }
    case "GIVEAWAY_UPDATED": {
      // Broadcast events carry shared state only — preserve whether I entered.
      const giveaway = action.payload;
      const myEntered = state.giveaways[giveaway.id]?.myEntered ?? false;
      // Maintain the active-widgets bar for the viewed channel: a giveaway
      // created live appends its chip; ended/cancelled drops it. 20-cap kept.
      let activeWidgets = state.activeWidgets;
      if (activeWidgets && giveaway.channel_id === activeWidgets.channelId) {
        if (giveaway.status !== "open") {
          if (activeWidgets.giveaways.includes(giveaway.id)) {
            activeWidgets = { ...activeWidgets, giveaways: activeWidgets.giveaways.filter((id) => id !== giveaway.id) };
          }
        } else if (
          !activeWidgets.giveaways.includes(giveaway.id) &&
          activeWidgets.polls.length + activeWidgets.giveaways.length + activeWidgets.events.length < ACTIVE_WIDGETS_CAP
        ) {
          activeWidgets = { ...activeWidgets, giveaways: [...activeWidgets.giveaways, giveaway.id] };
        }
      }
      return { ...state, activeWidgets, giveaways: { ...state.giveaways, [giveaway.id]: { giveaway, myEntered } } };
    }
    case "GIVEAWAY_STATE": {
      const { giveaway, myEntered } = action.payload;
      return { ...state, giveaways: { ...state.giveaways, [giveaway.id]: { giveaway, myEntered } } };
    }
    case "GIVEAWAY_MY_ENTERED": {
      const { giveawayId, myEntered } = action.payload;
      const existing = state.giveaways[giveawayId];
      if (!existing) return state;
      return { ...state, giveaways: { ...state.giveaways, [giveawayId]: { ...existing, myEntered } } };
    }
    case "EVENT_UPDATED": {
      // Broadcast events carry shared state only — preserve my existing RSVP.
      const event = action.payload;
      const myRsvp = state.events[event.id]?.myRsvp ?? null;
      // Maintain the active-widgets bar for the viewed channel: an event
      // created live appends its chip; started/cancelled drops it. 20-cap kept
      // across all three widget kinds.
      let activeWidgets = state.activeWidgets;
      if (activeWidgets && event.channel_id === activeWidgets.channelId) {
        if (event.status !== "upcoming") {
          if (activeWidgets.events.includes(event.id)) {
            activeWidgets = { ...activeWidgets, events: activeWidgets.events.filter((id) => id !== event.id) };
          }
        } else if (
          !activeWidgets.events.includes(event.id) &&
          activeWidgets.polls.length + activeWidgets.giveaways.length + activeWidgets.events.length < ACTIVE_WIDGETS_CAP
        ) {
          activeWidgets = { ...activeWidgets, events: [...activeWidgets.events, event.id] };
        }
      }
      return { ...state, activeWidgets, events: { ...state.events, [event.id]: { event, myRsvp } } };
    }
    case "EVENT_STATE": {
      const { event, myRsvp } = action.payload;
      return { ...state, events: { ...state.events, [event.id]: { event, myRsvp } } };
    }
    case "EVENT_MY_RSVP": {
      const { eventId, myRsvp } = action.payload;
      const existing = state.events[eventId];
      if (!existing) return state;
      return { ...state, events: { ...state.events, [eventId]: { ...existing, myRsvp } } };
    }
    case "ACTIVE_WIDGETS": {
      // Replace the bar's id lists whole and upsert every info into the shared
      // polls/giveaways/events slices with broadcast semantics (shared state
      // only — preserve any existing per-viewer myVote/myEntered/myRsvp).
      const { channelId, polls, giveaways, events } = action.payload;
      const pollsSlice = { ...state.polls };
      for (const p of polls) {
        pollsSlice[p.id] = { poll: p, myVote: state.polls[p.id]?.myVote ?? null };
      }
      const giveawaysSlice = { ...state.giveaways };
      for (const g of giveaways) {
        giveawaysSlice[g.id] = { giveaway: g, myEntered: state.giveaways[g.id]?.myEntered ?? false };
      }
      const eventsSlice = { ...state.events };
      for (const ev of events) {
        eventsSlice[ev.id] = { event: ev, myRsvp: state.events[ev.id]?.myRsvp ?? null };
      }
      return {
        ...state,
        polls: pollsSlice,
        giveaways: giveawaysSlice,
        events: eventsSlice,
        activeWidgets: {
          channelId,
          polls: polls.map((p) => p.id),
          giveaways: giveaways.map((g) => g.id),
          events: events.map((ev) => ev.id),
        },
      };
    }
    default:
      return state;
  }
}

function appReducer(state: AppState, action: AppAction): AppState {
  switch (action.type) {
    case "SET_IDENTITY":
      return { ...state, hasIdentity: true };

    case "SERVER_ADDED": {
      const { serverId, payload } = action;
      const existing = state.serverList.find((s) => s.id === serverId);
      const newEntry: ServerListEntry = existing ?? {
        id: serverId,
        name: payload.server_name,
        connected: true,
        unreadCount: 0,
        hasMention: false,
      };
      const updatedEntry: ServerListEntry = { ...newEntry, name: payload.server_name, connected: true };
      const serverList = existing
        ? state.serverList.map((s) => s.id === serverId ? updatedEntry : s)
        : [...state.serverList, updatedEntry];

      const existingServer = state.servers[serverId] ?? { ...initialPerServerState };
      const newPerServer: PerServerState = {
        ...existingServer,
        connected: true,
        connectionLost: false,
        serverName: payload.server_name,
        channels: payload.channels.map((c) => ({ ...c, class: c.class ?? "Plaintext" })),
        categories: payload.categories,
        roles: payload.roles,
        ownerPublicKey: payload.owner_public_key
          ? publicKeyToString(payload.owner_public_key)
          : null,
        relayed: payload.relayed ?? false,
        logServerId: payload.server_id ?? null,
      };

      return {
        ...state,
        serverList,
        servers: { ...state.servers, [serverId]: newPerServer },
      };
    }

    case "SERVER_REMOVED": {
      const { serverId } = action;
      const serverList = state.serverList.filter((s) => s.id !== serverId);
      const servers = { ...state.servers };
      delete servers[serverId];
      const activeServerId = state.activeServerId === serverId
        ? (serverList[0]?.id ?? null)
        : state.activeServerId;
      return { ...state, serverList, servers, activeServerId };
    }

    case "SET_ACTIVE_SERVER":
      return { ...state, activeServerId: action.serverId };

    case "UPDATE_SERVER_LIST":
      return { ...state, serverList: action.payload };

    case "INCREMENT_UNREAD": {
      const { serverId } = action;
      const serverList = state.serverList.map((s) =>
        s.id === serverId ? { ...s, unreadCount: s.unreadCount + 1 } : s,
      );
      return { ...state, serverList };
    }

    case "CLEAR_UNREAD": {
      const { serverId } = action;
      const serverList = state.serverList.map((s) =>
        s.id === serverId ? { ...s, unreadCount: 0, hasMention: false } : s,
      );
      return { ...state, serverList };
    }

    case "YOU_WERE_KICKED":
      return { ...state, kickedBanned: { kind: "kick", serverId: action.serverId, serverName: action.serverName, reason: null } };

    case "YOU_WERE_BANNED":
      return { ...state, kickedBanned: { kind: "ban", serverId: action.serverId, serverName: action.serverName, reason: action.reason } };

    case "CLEAR_KICKED_BANNED":
      return { ...state, kickedBanned: null };

    case "OPEN_JOIN_CONFIRM":
      return { ...state, joinConfirmLink: action.link };

    case "CLOSE_JOIN_CONFIRM":
      return { ...state, joinConfirmLink: null };

    default: {
      // Per-server actions — route to the appropriate server slice
      const serverId = (action as any).serverId as string | undefined;
      if (!serverId) return state;
      const existing = state.servers[serverId];
      if (!existing) return state;
      const updated = perServerReducer(existing, action);
      if (updated === existing) return state;

      // Sync serverList name/connected from per-server state when relevant
      let serverList = state.serverList;
      if (action.type === "CONNECTED" || action.type === "SERVER_REFRESHED") {
        serverList = state.serverList.map((s) =>
          s.id === serverId ? { ...s, name: updated.serverName, connected: true } : s,
        );
      } else if (action.type === "CONNECTION_LOST") {
        serverList = state.serverList.map((s) =>
          s.id === serverId ? { ...s, connected: false } : s,
        );
      } else if (action.type === "RECONNECTED") {
        serverList = state.serverList.map((s) =>
          s.id === serverId ? { ...s, connected: true } : s,
        );
      }

      return {
        ...state,
        serverList,
        servers: { ...state.servers, [serverId]: updated },
      };
    }
  }
}

interface AppContextValue {
  state: AppState;
  dispatch: React.Dispatch<AppAction>;
}

const AppContext = createContext<AppContextValue | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(appReducer, initialAppState);
  return <AppContext.Provider value={{ state, dispatch }}>{children}</AppContext.Provider>;
}

// Alias for backward compat
export const ServerProvider = AppProvider;

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp must be used inside AppProvider");
  return ctx;
}

// Alias for backward compat
export function useServer(): AppContextValue {
  return useApp();
}

export function useActiveServer(): PerServerState | null {
  const { state } = useApp();
  if (!state.activeServerId) return null;
  return state.servers[state.activeServerId] ?? null;
}

export function useActiveServerId(): string | null {
  const { state } = useApp();
  return state.activeServerId;
}
