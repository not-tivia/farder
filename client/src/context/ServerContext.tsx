import React, { createContext, useContext, useReducer, ReactNode } from "react";
import type { ChannelInfo, CategoryInfo, RoleInfo, MemberInfo, MessageInfo, ConnectResult, DmEntry, ServerListEntry } from "../lib/types";

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
}

export interface AppState {
  hasIdentity: boolean;
  activeServerId: string | null;
  serverList: ServerListEntry[];
  servers: Record<string, PerServerState>;
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
};

const initialAppState: AppState = {
  hasIdentity: false,
  activeServerId: null,
  serverList: [],
  servers: {},
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
  | { type: "SET_MESSAGES"; serverId: string; payload: { channelId: number; messages: MessageInfo[] } }
  | { type: "PREPEND_MESSAGES"; serverId: string; payload: { channelId: number; messages: MessageInfo[] } }
  | { type: "NEW_MESSAGE"; serverId: string; payload: MessageInfo }
  | { type: "MESSAGE_EDITED"; serverId: string; payload: MessageInfo }
  | { type: "MESSAGE_DELETED"; serverId: string; payload: { channelId: number; messageId: number } }
  | { type: "REACTION_ADDED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string; me: boolean } }
  | { type: "REACTION_REMOVED"; serverId: string; payload: { channelId: number; messageId: number; emoji: string } }
  | { type: "MEMBER_JOINED"; serverId: string; payload: MemberInfo }
  | { type: "MEMBER_LEFT"; serverId: string; payload: { publicKeyBytes: number[] } }
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
  | { type: "TYPING_EXPIRED"; serverId: string; payload: { channelId: number; publicKey: string } };

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
      };
    case "DISCONNECTED":
      return { ...initialPerServerState, connected: false };
    case "CONNECTION_LOST":
      return { ...state, connected: false, connectionLost: true };
    case "RECONNECTED":
      return { ...state, connected: true, connectionLost: false };
    case "SET_MEMBERS":
      return { ...state, members: action.payload };
    case "SELECT_CHANNEL": {
      const chMsgs = state.messages[action.payload] ?? [];
      const latestId = chMsgs.length > 0 ? Math.max(...chMsgs.map((m) => m.id)) : 0;
      const newReadState = latestId > 0
        ? { ...state.readState, [action.payload]: latestId }
        : state.readState;
      return { ...state, currentChannelId: action.payload, threadChannelId: null, readState: newReadState };
    }
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
      const channelId = action.payload.channel_id;
      const msgs = state.messages[channelId] ?? [];
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => (m.id === action.payload.id ? action.payload : m)),
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
      const { channelId, messageId, emoji, me } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => {
            if (m.id !== messageId) return m;
            const existing = m.reactions.find((r) => r.emoji === emoji);
            if (existing) {
              if (me && existing.me) return m;
              const reactions = m.reactions.map((r) =>
                r.emoji === emoji ? { ...r, count: r.count + 1, me: me || r.me } : r,
              );
              return { ...m, reactions };
            }
            return { ...m, reactions: [...m.reactions, { emoji, count: 1, me }] };
          }),
        },
      };
    }
    case "REACTION_REMOVED": {
      const { channelId, messageId, emoji } = action.payload;
      const msgs = state.messages[channelId] ?? [];
      return {
        ...state,
        messages: {
          ...state.messages,
          [channelId]: msgs.map((m) => {
            if (m.id !== messageId) return m;
            const reactions = m.reactions
              .map((r) => (r.emoji === emoji ? { ...r, count: r.count - 1 } : r))
              .filter((r) => r.count > 0);
            return { ...m, reactions };
          }),
        },
      };
    }
    case "MEMBER_JOINED":
      return { ...state, members: [...state.members, action.payload] };
    case "MEMBER_LEFT":
      return {
        ...state,
        members: state.members.filter(
          (m) => !m.public_key.bytes.every((b, i) => b === action.payload.publicKeyBytes[i]),
        ),
      };
    case "CHANNEL_CREATED":
      return { ...state, channels: [...state.channels, action.payload] };
    case "CHANNEL_DELETED":
      return {
        ...state,
        channels: state.channels.filter((c) => c.id !== action.payload.channelId),
      };
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
