import { useEffect, useState, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../lib/tauri-bridge";
import { publicKeyToString } from "../lib/types";

export interface VoiceUiPeer {
  pubkey: string;        // publicKeyToString(peer.pubkey)
  speaking: boolean;
  muted: boolean;
  deafened: boolean;
}

export interface UseVoice {
  inCall: boolean;
  muted: boolean;
  deafened: boolean;
  localSpeaking: boolean;
  peers: VoiceUiPeer[];
  join: (serverId: string, channelId: number) => Promise<void>;
  leave: () => Promise<void>;
  setMute: (muted: boolean) => Promise<void>;
  setDeafen: (deafened: boolean) => Promise<void>;
}

function normalize(state: api.VoiceState): { inCall: boolean; peers: VoiceUiPeer[] } {
  return {
    inCall: state.channel_id !== null,
    peers: state.peers.map((p) => ({
      pubkey: publicKeyToString(p.pubkey),
      speaking: p.speaking,
      muted: p.muted,
      deafened: p.deafened,
    })),
  };
}

export function useVoice(): UseVoice {
  const [inCall, setInCall] = useState(false);
  const [muted, setMutedState] = useState(false);
  const [deafened, setDeafenedState] = useState(false);
  const [localSpeaking, setLocalSpeaking] = useState(false);
  const [peers, setPeers] = useState<VoiceUiPeer[]>([]);

  const applyState = useCallback((s: api.VoiceState) => {
    const n = normalize(s);
    setInCall(n.inCall);
    setMutedState(s.muted);
    setDeafenedState(s.deafened);
    setPeers(n.peers);
    if (!n.inCall) setLocalSpeaking(false);
  }, []);

  useEffect(() => {
    let cleanupRan = false;
    const unlisten: Array<() => void> = [];
    const safePush = (u: () => void) => { if (cleanupRan) u(); else unlisten.push(u); };

    api.voiceGetState().then(applyState).catch(() => {});

    listen<api.VoiceState>("voice://state-changed", (e) => applyState(e.payload)).then(safePush);
    listen<api.VoiceLocalSpeakingPayload>("voice://local-speaking", (e) =>
      setLocalSpeaking(e.payload.speaking)).then(safePush);
    listen<api.VoicePeerSpeakingPayload>("voice://peer-speaking", (e) => {
      setPeers((prev) => prev.map((p) =>
        p.pubkey === e.payload.pubkey ? { ...p, speaking: e.payload.active } : p));
    }).then(safePush);

    return () => { cleanupRan = true; unlisten.forEach((u) => u()); };
  }, [applyState]);

  const join = useCallback((serverId: string, channelId: number) => api.voiceJoin(serverId, channelId), []);
  const leave = useCallback(() => api.voiceLeave(), []);
  const setMute = useCallback((m: boolean) => api.voiceSetMute(m), []);
  const setDeafen = useCallback((d: boolean) => api.voiceSetDeafen(d), []);

  return { inCall, muted, deafened, localSpeaking, peers, join, leave, setMute, setDeafen };
}
