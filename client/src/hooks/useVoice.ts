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
  peerVolume: (pubkey: string) => number;
  setPeerVolume: (pubkey: string, v: number) => Promise<void>;
  isSharing: boolean;
  startShare: () => Promise<void>;
  stopShare: () => Promise<void>;
  audioDevices: api.AudioOutputDevice[];
  audioDeviceId: string | null;
  setAudioDeviceId: (id: string | null) => void;
  displaySources: api.DisplaySource[];
  sourceId: string | null;
  setSourceId: (id: string | null) => void;
  someoneElseSharing: boolean;
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
  const [peerVolumes, setPeerVolumes] = useState<Record<string, number>>({});
  // Known C2 limitation: if the backend capture source terminates on its own
  // (window closed, display unplugged), the send thread exits but no event is
  // emitted, so isSharing stays true until the user stops manually or leaves.
  // Phase E adds a backend-stop event to reconcile this.
  const [isSharing, setIsSharing] = useState(false);
  const [audioDevices, setAudioDevices] = useState<api.AudioOutputDevice[]>([]);
  const [audioDeviceId, setAudioDeviceId] = useState<string | null>(null);
  useEffect(() => {
    api.listAudioOutputDevices().then((d) => {
      setAudioDevices(d);
      const def = d.find((x) => x.is_default);
      setAudioDeviceId((cur) => cur ?? def?.id ?? d[0]?.id ?? null);
    }).catch(() => {});
  }, []);
  const [displaySources, setDisplaySources] = useState<api.DisplaySource[]>([]);
  const [sourceId, setSourceId] = useState<string | null>(null);
  useEffect(() => {
    api.listDisplaySources().then((s) => {
      setDisplaySources(s);
      setSourceId((cur) => cur ?? s[0]?.id ?? null);
    }).catch(() => {});
  }, []);

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
      setIsSharing(false);
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

  const startShare = useCallback(async () => {
    // No catch: a start failure leaves isSharing=false (correct); the rejection
    // surfaces as an unhandledrejection, consistent with toggleTransmit. Phase E
    // adds user-facing error feedback.
    await api.voiceStartScreenShare(30, 1280, 720, sourceId, audioDeviceId);
    setIsSharing(true);
  }, [sourceId, audioDeviceId]);
  const stopShare = useCallback(async () => {
    try { await api.voiceStopScreenShare(); } catch {}
    setIsSharing(false);
  }, []);

  const join = useCallback((serverId: string, channelId: number) => api.voiceJoin(serverId, channelId), []);
  const leave = useCallback(() => api.voiceLeave(), []);
  const setMute = useCallback((m: boolean) => api.voiceSetMute(m), []);
  const setDeafen = useCallback((d: boolean) => api.voiceSetDeafen(d), []);
  const toggleTransmit = useCallback(async () => {
    await api.voiceToggleTransmit();
    // State refreshes via the existing voice://state-changed listener.
  }, []);

  // Task 5 wires this from the sharingPeers set; default false until then.
  const someoneElseSharing = false;

  // Per-peer playback volume (gain), keyed by pubkey string (the same
  // publicKeyToString / PublicKey::to_string() form the backend keys by).
  // Loaded once from persisted settings; updates are optimistic so the slider
  // reflects changes immediately while the backend clamps + persists.
  useEffect(() => { void api.getPeerVolumes().then(setPeerVolumes).catch(() => {}); }, []);
  const peerVolume = useCallback(
    (pubkey: string) => peerVolumes[pubkey] ?? 1.0,
    [peerVolumes],
  );
  const setPeerVolume = useCallback(async (pubkey: string, v: number) => {
    const clamped = Math.max(0, Math.min(2, v));
    setPeerVolumes((prev) => ({ ...prev, [pubkey]: clamped }));
    await api.voiceSetPeerVolume(pubkey, clamped);
  }, []);

  return { inCall, muted, deafened, transmitting, localSpeaking, peers, join, leave, setMute, setDeafen, toggleTransmit, connectionQuality, peerVolume, setPeerVolume, isSharing, startShare, stopShare, audioDevices, audioDeviceId, setAudioDeviceId, displaySources, sourceId, setSourceId, someoneElseSharing };
}
