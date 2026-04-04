import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useServer } from "../context/ServerContext";
import type { MessageInfo, MemberInfo, ChannelInfo, CategoryInfo } from "../lib/types";

interface ReactionAddedPayload {
  channel_id: number;
  message_id: number;
  emoji: string;
  me: boolean;
}

interface ReactionRemovedPayload {
  channel_id: number;
  message_id: number;
  emoji: string;
}

interface MemberLeftPayload {
  public_key_bytes: number[];
}

interface ChannelDeletedPayload {
  channel_id: number;
}

interface MessageDeletedPayload {
  channel_id: number;
  message_id: number;
}

export function useServerEvents(): void {
  const { dispatch } = useServer();

  useEffect(() => {
    const unlisten: Array<() => void> = [];

    listen<MessageInfo>("server:new_message", (e) => {
      dispatch({ type: "NEW_MESSAGE", payload: e.payload });
    }).then((u) => unlisten.push(u));

    listen<MessageInfo>("server:message_edited", (e) => {
      dispatch({ type: "MESSAGE_EDITED", payload: e.payload });
    }).then((u) => unlisten.push(u));

    listen<MessageDeletedPayload>("server:message_deleted", (e) => {
      dispatch({
        type: "MESSAGE_DELETED",
        payload: { channelId: e.payload.channel_id, messageId: e.payload.message_id },
      });
    }).then((u) => unlisten.push(u));

    listen<ReactionAddedPayload>("server:reaction_added", (e) => {
      dispatch({
        type: "REACTION_ADDED",
        payload: {
          channelId: e.payload.channel_id,
          messageId: e.payload.message_id,
          emoji: e.payload.emoji,
          me: e.payload.me,
        },
      });
    }).then((u) => unlisten.push(u));

    listen<ReactionRemovedPayload>("server:reaction_removed", (e) => {
      dispatch({
        type: "REACTION_REMOVED",
        payload: {
          channelId: e.payload.channel_id,
          messageId: e.payload.message_id,
          emoji: e.payload.emoji,
        },
      });
    }).then((u) => unlisten.push(u));

    listen<MemberInfo>("server:member_joined", (e) => {
      dispatch({ type: "MEMBER_JOINED", payload: e.payload });
    }).then((u) => unlisten.push(u));

    listen<MemberLeftPayload>("server:member_left", (e) => {
      dispatch({ type: "MEMBER_LEFT", payload: { publicKeyBytes: e.payload.public_key_bytes } });
    }).then((u) => unlisten.push(u));

    listen<ChannelInfo>("server:channel_created", (e) => {
      dispatch({ type: "CHANNEL_CREATED", payload: e.payload });
    }).then((u) => unlisten.push(u));

    listen<ChannelDeletedPayload>("server:channel_deleted", (e) => {
      dispatch({ type: "CHANNEL_DELETED", payload: { channelId: e.payload.channel_id } });
    }).then((u) => unlisten.push(u));

    listen<CategoryInfo>("server:category_created", (e) => {
      dispatch({ type: "CATEGORY_CREATED", payload: e.payload });
    }).then((u) => unlisten.push(u));

    listen<{ category_id: number }>("server:category_deleted", (e) => {
      dispatch({ type: "CATEGORY_DELETED", payload: { categoryId: e.payload.category_id } });
    }).then((u) => unlisten.push(u));

    listen<void>("server:disconnected", () => {
      dispatch({ type: "CONNECTION_LOST" });
    }).then((u) => unlisten.push(u));

    return () => {
      unlisten.forEach((u) => u());
    };
  }, [dispatch]);
}
