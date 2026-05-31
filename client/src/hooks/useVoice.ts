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
  transmitting: boolean;
  localSpeaking: boolean;
  peers: VoiceUiPeer[];
  join: (serverId: string, channelId: number) => Promise<void>;
  leave: () => Promise<void>;
  setMute: (muted: boolean) => Promise<void>;
  setDeafen: (deafened: boolean) => Promise<void>;
  toggleTransmit: () => Promise<void>;
  connectionQuality: { rttMs: number; lossPct: number } | null;
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
  const [transmitting, setTransmitting] = useState(false);
  const [localSpeaking, setLocalSpeaking] = useState(false);
  const [peers, setPeers] = useState<VoiceUiPeer[]>([]);
  const [connectionQuality, setConnectionQuality] =
    useState<{ rttMs: number; lossPct: number } | null>(null);

  const applyState = useCallback((s: api.VoiceState) => {
    const n = normalize(s);
    setInCall(n.inCall);
    setMutedState(s.muted);
    setDeafenedState(s.deafened);
    setTransmitting(s.transmitting);
    setPeers(n.peers);
    if (!n.inCall) {
      setLocalSpeaking(false);
      setTransmitting(false);
      setConnectionQuality(null);
    }
  }, []);

  useEffect(() => {
    let cleanupRan = false;
    const unlisten: Array<() => void> = [];
    // If cleanup already ran before a listen() promise resolved (StrictMode
    // double-mount), unlisten immediately instead of leaking the handler.
    const safePush = (u: () => void) => { if (cleanupRan) u(); else unlisten.push(u); };

    api.voiceGetState().then(applyState).catch(() => {});

    listen<api.VoiceState>("voice://state-changed", (e) => applyState(e.payload)).then(safePush);
    listen<api.VoiceLocalSpeakingPayload>("voice://local-speaking", (e) =>
      setLocalSpeaking(e.payload.speaking)).then(safePush);
    listen<api.VoicePeerSpeakingPayload>("voice://peer-speaking", (e) => {
      // If the peer isn't seeded yet (event raced ahead of state-changed),
      // this is a no-op; the next state-changed fills in the speaking flag.
      setPeers((prev) => prev.map((p) =>
        p.pubkey === e.payload.pubkey ? { ...p, speaking: e.payload.active } : p));
    }).then(safePush);
    listen<api.ConnectionQualityPayload>("voice://connection-quality", (e) =>
      setConnectionQuality({ rttMs: e.payload.rtt_ms, lossPct: e.payload.loss_pct })).then(safePush);

    return () => { cleanupRan = true; unlisten.forEach((u) => u()); };
  }, [applyState]);

  const join = useCallback((serverId: string, channelId: number) => api.voiceJoin(serverId, channelId), []);
  const leave = useCallback(() => api.voiceLeave(), []);
  const setMute = useCallback((m: boolean) => api.voiceSetMute(m), []);
  const setDeafen = useCallback((d: boolean) => api.voiceSetDeafen(d), []);
  const toggleTransmit = useCallback(async () => {
    await api.voiceToggleTransmit();
    // State refreshes via the existing voice://state-changed listener.
  }, []);

  return { inCall, muted, deafened, transmitting, localSpeaking, peers, join, leave, setMute, setDeafen, toggleTransmit, connectionQuality };
}
