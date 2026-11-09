import type { Dispatch } from "react";
import type { AppAction } from "../context/ServerContext";
import * as api from "./tauri-bridge";
import { publicKeyToString, flattenChannelInfoV2 } from "./types";

/** Switch to a server and load what the screens need (info, members, DMs, voice rosters). */
export async function selectServer(dispatch: Dispatch<AppAction>, serverId: string): Promise<void> {
  dispatch({ type: "SET_ACTIVE_SERVER", serverId });
  dispatch({ type: "CLEAR_UNREAD", serverId });
  void api.setLastServer(serverId);
  try {
    const info = await api.getServerInfoV2(serverId);
    dispatch({ type: "SERVER_REFRESHED", serverId, payload: info });
    const members = await api.getMembers(serverId);
    dispatch({ type: "SET_MEMBERS", serverId, payload: members });
    const dms = await api.listDms(serverId);
    dispatch({ type: "SET_DMS", serverId, payload: dms });
    // Load voice states for all voice channels
    const flatChannels = flattenChannelInfoV2(info.channels);
    const voiceChannels = flatChannels.filter((c) => c.channel_type === "Voice");
    for (const vc of voiceChannels) {
      try {
        const vs = await api.getVoiceState(serverId, vc.id);
        const simplified = (vs || []).filter((v: any) => v && v.public_key).map((v: any) => ({
          publicKey: typeof v.public_key === "string" ? v.public_key : publicKeyToString(v.public_key),
          displayName: v.display_name || "Unknown",
        }));
        dispatch({ type: "SET_VOICE_STATE", serverId, payload: { channelId: vc.id, participants: simplified } });
      } catch {}
    }
  } catch (e) {
    console.error("[server] failed to load server data:", e);
  }
}
