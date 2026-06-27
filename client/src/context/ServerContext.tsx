import React, { createContext, useContext, useReducer, ReactNode } from "react";
import type { ChannelInfo, CategoryInfo, RoleInfo, MemberInfo, MessageInfo, ConnectResult, DmEntry, ServerListEntry, Presence } from "../lib/types";
import { publicKeyToString } from "../lib/types";

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
}

export interface AppState {
  hasIdentity: boolean;
  activeServerId: string | null;
  serverList: ServerListEntry[];
  servers: Record<string, PerServerState>;
  kickedBanned: { kind: "kick" | "ban"; serverId: string; reason: string | null } | null;
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
};

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
  | { type: "CONNECTED"; serverId: string; payload: ConnectResult }
  | { type: "SERVER_REFRESHED"; serverId: string; payload: ConnectResult }
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
  | { type: "VOICE_JOINED"; serverId: string; payload: { channelId: number; publicKey: string; displayName: string } }
  | { type: "VOICE_LEFT"; serverId: string; payload: { channelId: number; publicKey: string } }
  | { type: "SET_VOICE_STATE"; serverId: string; payload: { channelId: number; participants: { publicKey: string; displayName: string }[] } }
  | { type: "JOIN_VOICE_CHANNEL"; serverId: string; payload: number }
  | { type: "LEAVE_VOICE_CHANNEL"; serverId: string }
  | { type: "MEMBER_TIMEOUT_CHANGED"; serverId: string; payload: { publicKey: string; untilMs: number | null; reason: string | null } }
  | { type: "UPDATE_MEMBER_PRESENCE"; serverId: string; payload: { publicKey: string; presence: Presence | null } }
  | { type: "YOU_WERE_KICKED"; serverId: string }
  | { type: "YOU_WERE_BANNED"; serverId: string; reason: string | null }
  | { type: "CLEAR_KICKED_BANNED" }
  | { type: "OPEN_JOIN_CONFIRM"; link: string }
  | { type: "CLOSE_JOIN_CONFIRM" };

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
        channels: action.payload.channels,
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
    case "CHANNEL_CREATED":
      if (state.channels.some(c => c.id === action.payload.id)) return state;
      return { ...state, channels: [...state.channels, action.payload] };
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
      return { ...state, channels: state.channels.map((c) => c.id === action.payload.id ? action.payload : c) };
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
        channels: payload.channels,
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
      return { ...state, kickedBanned: { kind: "kick", serverId: action.serverId, reason: null } };

    case "YOU_WERE_BANNED":
      return { ...state, kickedBanned: { kind: "ban", serverId: action.serverId, reason: action.reason } };

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
