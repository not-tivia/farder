import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useApp } from "../context/ServerContext";
import type { MessageInfo, MemberInfo, ChannelInfo, CategoryInfo } from "../lib/types";

interface ReactionAddedPayload {
  server_id: string;
  channel_id: number;
  message_id: number;
  emoji: string;
  me: boolean;
}

interface ReactionRemovedPayload {
  server_id: string;
  channel_id: number;
  message_id: number;
  emoji: string;
}

interface MemberLeftPayload {
  server_id: string;
  public_key_bytes: number[];
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

  useEffect(() => {
    const unlisten: Array<() => void> = [];

    listen("server:new_message", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      const message = data.message as MessageInfo;
      if (serverId === activeRef.current) {
        dispatch({ type: "NEW_MESSAGE", serverId, payload: message });
      } else {
        dispatch({ type: "INCREMENT_UNREAD", serverId });
      }
    }).then((u) => unlisten.push(u));

    listen("server:message_edited", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "MESSAGE_EDITED", serverId, payload: data.message as MessageInfo });
    }).then((u) => unlisten.push(u));

    listen("server:message_deleted", (e) => {
      const data = e.payload as MessageDeletedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({
        type: "MESSAGE_DELETED",
        serverId,
        payload: { channelId: data.channel_id, messageId: data.message_id },
      });
    }).then((u) => unlisten.push(u));

    listen("server:reaction_added", (e) => {
      const data = e.payload as ReactionAddedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({
        type: "REACTION_ADDED",
        serverId,
        payload: {
          channelId: data.channel_id,
          messageId: data.message_id,
          emoji: data.emoji,
          me: data.me,
        },
      });
    }).then((u) => unlisten.push(u));

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
        },
      });
    }).then((u) => unlisten.push(u));

    listen("server:member_joined", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "MEMBER_JOINED", serverId, payload: data.member as MemberInfo });
    }).then((u) => unlisten.push(u));

    listen("server:member_left", (e) => {
      const data = e.payload as MemberLeftPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "MEMBER_LEFT", serverId, payload: { publicKeyBytes: data.public_key_bytes } });
    }).then((u) => unlisten.push(u));

    listen("server:channel_created", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CHANNEL_CREATED", serverId, payload: data.channel as ChannelInfo });
    }).then((u) => unlisten.push(u));

    listen("server:channel_deleted", (e) => {
      const data = e.payload as ChannelDeletedPayload;
      const serverId = data.server_id;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CHANNEL_DELETED", serverId, payload: { channelId: data.channel_id } });
    }).then((u) => unlisten.push(u));

    listen("server:category_created", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CATEGORY_CREATED", serverId, payload: data.category as CategoryInfo });
    }).then((u) => unlisten.push(u));

    listen("server:category_deleted", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CATEGORY_DELETED", serverId, payload: { categoryId: data.category_id as number } });
    }).then((u) => unlisten.push(u));

    listen("server:category_updated", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CATEGORY_UPDATED", serverId, payload: data.category as CategoryInfo });
    }).then((u) => unlisten.push(u));

    listen("server:channel_updated", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "CHANNEL_UPDATED", serverId, payload: data.channel as ChannelInfo });
    }).then((u) => unlisten.push(u));

    listen("server:disconnected", (e) => {
      const data = e.payload as any;
      dispatch({ type: "CONNECTION_LOST", serverId: data.server_id as string });
    }).then((u) => unlisten.push(u));

    listen("server:dm_created", (e) => {
      const data = e.payload as any;
      const serverId = data.server_id as string;
      if (serverId !== activeRef.current) return;
      dispatch({ type: "DM_CREATED", serverId, payload: { channel: data.channel, participant: data.participant } });
    }).then((u) => unlisten.push(u));

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
    }).then((u) => unlisten.push(u));

    return () => {
      unlisten.forEach((u) => u());
    };
  }, [dispatch]);
}
