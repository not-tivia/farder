import { useEffect, useRef } from "react";
import { useApp, useActiveServer, useActiveServerId } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import Message from "./Message";
import MessageInput from "./MessageInput";

export default function DmPanel() {
  const { dispatch } = useApp();
  const activeServer = useActiveServer();
  const serverId = useActiveServerId();
  const channelId = activeServer?.dmPanelChannelId ?? null;
  const bottomRef = useRef<HTMLDivElement>(null);

  const dms = activeServer?.dms ?? [];
  const messages = activeServer?.messages ?? {};
  const members = activeServer?.members ?? [];

  const dm = dms.find(d => d.channel.id === channelId);
  const dmMessages = channelId ? (messages[channelId] ?? []) : [];

  const memberNames: Record<string, string> = {};
  for (const m of members) {
    memberNames[publicKeyToString(m.public_key)] = m.display_name;
  }
  if (dm) {
    memberNames[publicKeyToString(dm.participant.public_key)] = dm.participant.display_name;
  }

  useEffect(() => {
    if (!channelId || !serverId) return;
    (async () => {
      const msgs = await api.fetchHistory(serverId, channelId);
      dispatch({ type: "SET_MESSAGES", serverId: serverId!, payload: { channelId, messages: msgs.reverse() } });
    })();
  }, [channelId, serverId]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [dmMessages.length]);

  if (!channelId || !dm || !serverId) return null;

  return (
    <div className="dm-panel">
      <div className="dm-panel-header">
        <span>{dm.participant.display_name}</span>
        <button className="modal-close" onClick={() => dispatch({ type: "CLOSE_DM_PANEL", serverId: serverId! })}>X</button>
      </div>
      <div className="dm-panel-messages">
        {dmMessages.map((msg, i) => {
          const prev = i > 0 ? dmMessages[i - 1] : null;
          const sameAuthor = prev && JSON.stringify(prev.author.bytes) === JSON.stringify(msg.author.bytes);
          const withinWindow = prev && (msg.timestamp - prev.timestamp) < 300;
          const grouped = !!(sameAuthor && withinWindow);
          return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} serverId={serverId} />;
        })}
        <div ref={bottomRef} />
      </div>
      <MessageInput channelId={channelId} serverId={serverId} />
    </div>
  );
}
