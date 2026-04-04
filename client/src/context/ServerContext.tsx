import React, { createContext, useContext, useReducer, ReactNode } from "react";
import type { ChannelInfo, CategoryInfo, RoleInfo, MemberInfo, MessageInfo, ConnectResult } from "../lib/types";

export interface ServerState {
  connected: boolean;
  serverName: string;
  channels: ChannelInfo[];
  categories: CategoryInfo[];
  roles: RoleInfo[];
  members: MemberInfo[];
  currentChannelId: number | null;
  messages: Record<number, MessageInfo[]>;
  threadChannelId: number | null;
}

const initialState: ServerState = {
  connected: false,
  serverName: "",
  channels: [],
  categories: [],
  roles: [],
  members: [],
  currentChannelId: null,
  messages: {},
  threadChannelId: null,
};

export type ServerAction =
  | { type: "CONNECTED"; payload: ConnectResult }
  | { type: "DISCONNECTED" }
  | { type: "SET_MEMBERS"; payload: MemberInfo[] }
  | { type: "SELECT_CHANNEL"; payload: number }
  | { type: "SET_MESSAGES"; payload: { channelId: number; messages: MessageInfo[] } }
  | { type: "PREPEND_MESSAGES"; payload: { channelId: number; messages: MessageInfo[] } }
  | { type: "NEW_MESSAGE"; payload: MessageInfo }
  | { type: "MESSAGE_EDITED"; payload: MessageInfo }
  | { type: "MESSAGE_DELETED"; payload: { channelId: number; messageId: number } }
  | { type: "REACTION_ADDED"; payload: { channelId: number; messageId: number; emoji: string; me: boolean } }
  | { type: "REACTION_REMOVED"; payload: { channelId: number; messageId: number; emoji: string } }
  | { type: "MEMBER_JOINED"; payload: MemberInfo }
  | { type: "MEMBER_LEFT"; payload: { publicKeyBytes: number[] } }
  | { type: "CHANNEL_CREATED"; payload: ChannelInfo }
  | { type: "CHANNEL_DELETED"; payload: { channelId: number } }
  | { type: "VIEW_THREAD"; payload: number | null };

function reducer(state: ServerState, action: ServerAction): ServerState {
  switch (action.type) {
    case "CONNECTED":
      return {
        ...state,
        connected: true,
        serverName: action.payload.server_name,
        channels: action.payload.channels,
        categories: action.payload.categories,
        roles: action.payload.roles,
        members: [],
        currentChannelId: null,
        messages: {},
        threadChannelId: null,
      };
    case "DISCONNECTED":
      return { ...initialState };
    case "SET_MEMBERS":
      return { ...state, members: action.payload };
    case "SELECT_CHANNEL":
      return { ...state, currentChannelId: action.payload, threadChannelId: null };
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
            const reactions = existing
              ? m.reactions.map((r) =>
                  r.emoji === emoji ? { ...r, count: r.count + 1, me: me || r.me } : r,
                )
              : [...m.reactions, { emoji, count: 1, me }];
            return { ...m, reactions };
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
    case "VIEW_THREAD":
      return { ...state, threadChannelId: action.payload };
    default:
      return state;
  }
}

interface ServerContextValue {
  state: ServerState;
  dispatch: React.Dispatch<ServerAction>;
}

const ServerContext = createContext<ServerContextValue | null>(null);

export function ServerProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initialState);
  return <ServerContext.Provider value={{ state, dispatch }}>{children}</ServerContext.Provider>;
}

export function useServer(): ServerContextValue {
  const ctx = useContext(ServerContext);
  if (!ctx) throw new Error("useServer must be used inside ServerProvider");
  return ctx;
}
