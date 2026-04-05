import { useEffect, useRef } from "react";
import { useServer } from "../context/ServerContext";
import { publicKeyToString } from "../lib/types";
import * as api from "../lib/tauri-bridge";
import Message from "./Message";
import MessageInput from "./MessageInput";

export default function DmPanel() {
    const { state, dispatch } = useServer();
    const channelId = state.dmPanelChannelId;
    const bottomRef = useRef<HTMLDivElement>(null);

    const dm = state.dms.find(d => d.channel.id === channelId);
    const messages = channelId ? (state.messages[channelId] ?? []) : [];

    const memberNames: Record<string, string> = {};
    for (const m of state.members) {
        memberNames[publicKeyToString(m.public_key)] = m.display_name;
    }
    if (dm) {
        memberNames[publicKeyToString(dm.participant.public_key)] = dm.participant.display_name;
    }

    useEffect(() => {
        if (!channelId) return;
        (async () => {
            // subscribeChannels is handled centrally by AppShell
            const msgs = await api.fetchHistory(channelId);
            dispatch({ type: "SET_MESSAGES", payload: { channelId, messages: msgs.reverse() } });
        })();
    }, [channelId]);

    useEffect(() => {
        bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }, [messages.length]);

    if (!channelId || !dm) return null;

    return (
        <div className="dm-panel">
            <div className="dm-panel-header">
                <span>{dm.participant.display_name}</span>
                <button className="modal-close" onClick={() => dispatch({ type: "CLOSE_DM_PANEL" })}>X</button>
            </div>
            <div className="dm-panel-messages">
                {messages.map((msg, i) => {
                    const prev = i > 0 ? messages[i - 1] : null;
                    const sameAuthor = prev && JSON.stringify(prev.author.bytes) === JSON.stringify(msg.author.bytes);
                    const withinWindow = prev && (msg.timestamp - prev.timestamp) < 300;
                    const grouped = !!(sameAuthor && withinWindow);
                    return <Message key={msg.id} message={msg} memberNames={memberNames} grouped={grouped} />;
                })}
                <div ref={bottomRef} />
            </div>
            <MessageInput channelId={channelId} />
        </div>
    );
}
