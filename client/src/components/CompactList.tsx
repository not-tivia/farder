import { useState } from "react";
import { useApp, useActiveServer, useActiveServerId } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";
import { isE2eeChannel, publicKeyToString } from "../lib/types";
import type { ChannelInfo } from "../lib/types";
import { selectChannel } from "../lib/selectChannel";
import { selectServer } from "../lib/selectServer";
import { changeLayout } from "../lib/layoutWindow";
import type { UseVoice } from "../hooks/useVoice";
import SettingsModal from "./settings/SettingsModal";
import AddServerModal from "./AddServerModal";

/**
 * The conversation list of the small-window layout: people (direct messages) and
 * channels of the current server in one narrow column, with a server switcher on
 * top. It REUSES the channel list's classes so it looks like the sidebar in every
 * theme. The full sidebar stays mounted (hidden by CSS) because it also keeps the
 * selected channel valid and the voice/reconnect logic running.
 */
export default function CompactList({ voice }: { voice: UseVoice }) {
  const { state, dispatch } = useApp();
  const server = useActiveServer();
  const serverId = useActiveServerId();
  const [pickServer, setPickServer] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [showAdd, setShowAdd] = useState(false);

  const channels = server?.channels ?? [];
  const messages = server?.messages ?? {};
  const readState = server?.readState ?? {};
  const currentChannelId = server?.currentChannelId ?? null;

  const textChannels = channels
    .filter((c) => c.channel_type === "Text" || c.channel_type === "Announcement")
    .sort((a, b) => a.position - b.position);
  // People first, the one who wrote last on top.
  const lastId = (id: number) => (messages[id] ?? []).reduce((m, x) => Math.max(m, x.id), 0);
  const dms = [...(server?.dms ?? [])].sort((a, b) => lastId(b.channel.id) - lastId(a.channel.id));

  const unread = (id: number) => id !== currentChannelId && (messages[id] ?? []).some((m) => m.id > (readState[id] ?? 0));

  function open(ch: ChannelInfo) {
    if (serverId) void selectChannel(dispatch, serverId, ch);
  }

  async function leaveVoice() {
    if (!serverId || server?.currentVoiceChannelId == null) return;
    const channelId = server.currentVoiceChannelId;
    await voice.leave().catch(() => {});
    await api.leaveVoice(serverId, channelId).catch(() => {});
    dispatch({ type: "LEAVE_VOICE_CHANNEL", serverId });
  }

  return (
    <div className="channel-sidebar compact-list">
      <div className="server-header">
        <div
          className="server-name"
          style={{ cursor: "pointer", flex: 1, minWidth: 0 }}
          role="button"
          tabIndex={0}
          title="Switch server"
          onClick={() => setPickServer((v) => !v)}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setPickServer((v) => !v); } }}
        >
          {server?.serverName ?? "Farder"} {pickServer ? "▴" : "▾"}
        </div>
        <button className="server-invite-btn" onClick={() => setShowSettings(true)} title="Settings">&#9881;</button>
      </div>
      <div className="channel-list">
        {pickServer && (
          <>
            <div className="channel-category">SERVERS</div>
            {state.serverList.map((s) => (
              <div
                key={s.id}
                className={`channel-item${s.id === serverId ? " active" : ""}${s.unreadCount > 0 && s.id !== serverId ? " unread" : ""}`}
                onClick={() => { setPickServer(false); void selectServer(dispatch, s.id); }}
              >
                <span>{s.name}</span>
              </div>
            ))}
            <div className="channel-item" onClick={() => { setPickServer(false); setShowAdd(true); }}>
              <span>+ Add server</span>
            </div>
          </>
        )}
        {!pickServer && dms.length > 0 && (
          <>
            <div className="channel-category">DIRECT MESSAGES</div>
            {dms.map((dm) => (
              <div
                key={dm.channel.id}
                className={`channel-item dm-item${currentChannelId === dm.channel.id ? " active" : ""}${unread(dm.channel.id) ? " unread" : ""}`}
                onClick={() => open(dm.channel)}
                title={publicKeyToString(dm.participant.public_key).slice(0, 12)}
              >
                <span className="dm-avatar-mini">{(dm.participant.display_name || "?").charAt(0).toUpperCase()}</span>
                <span>{dm.participant.display_name}</span>
              </div>
            ))}
          </>
        )}
        {!pickServer && (
          <>
            <div className="channel-category">CHANNELS</div>
            {textChannels.map((ch) => (
              <div
                key={ch.id}
                className={`channel-item${currentChannelId === ch.id ? " active" : ""}${unread(ch.id) ? " unread" : ""}`}
                onClick={() => open(ch)}
              >
                <span className="channel-prefix">{isE2eeChannel(ch) ? "🔒" : "#"}</span>
                <span>{ch.name}</span>
              </div>
            ))}
          </>
        )}
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 4, padding: 6 }}>
        {server?.currentVoiceChannelId != null && (
          <button className="xp-button" onClick={() => void leaveVoice()}>Leave voice</button>
        )}
        <button className="xp-button" onClick={() => changeLayout("full")} title="Back to the full window">Full window</button>
      </div>
      {showSettings && <SettingsModal onClose={() => setShowSettings(false)} />}
      {showAdd && <AddServerModal onClose={() => setShowAdd(false)} />}
    </div>
  );
}
