import type { Dispatch } from "react";
import type { AppAction } from "../context/ServerContext";
import * as api from "./tauri-bridge";
import type { ChannelInfo } from "./types";
import { flattenMessageInfoV2, isE2eeChannel } from "./types";
import { setLastChannel } from "./lastChannel";

/**
 * Open a channel (or a DM, which is a channel too): select it, remember it, load
 * its history and mark it read. Shared by the channel list in the full layout and
 * the conversation list in the small-window layout.
 */
export async function selectChannel(dispatch: Dispatch<AppAction>, serverId: string, channel: ChannelInfo): Promise<void> {
  dispatch({ type: "SELECT_CHANNEL", serverId, payload: channel.id });
  setLastChannel(serverId, channel.id);
  try {
    const msgs = isE2eeChannel(channel)
      ? flattenMessageInfoV2(await api.fetchHistoryV2(serverId, channel.id))
      : await api.fetchHistory(serverId, channel.id);
    const reversed = msgs.reverse();
    dispatch({ type: "SET_MESSAGES", serverId, payload: { channelId: channel.id, messages: reversed } });
    if (reversed.length > 0) {
      const latestId = Math.max(...reversed.map((m) => m.id));
      dispatch({ type: "MARK_READ", serverId, payload: { channelId: channel.id, lastMessageId: latestId } });
    }
  } catch (e) {
    console.error("[channel] failed to load history:", e);
  }
}
