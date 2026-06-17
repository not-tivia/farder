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
  sharingPeers: Set<string>;
  watching: Set<string>;
  toggleWatch: (pubkey: string) => void;
  setGameAudioVolume: (pubkey: string, gain: number) => void;
  refreshDisplaySources: () => Promise<void>;
  shareFps: number;
  setShareFps: (fps: number) => void;
  shareResId: "720" | "1080" | "source";
  setShareResId: (id: "720" | "1080" | "source") => void;
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
  const [sharingPeers, setSharingPeers] = useState<Set<string>>(new Set());
  const [watching, setWatching] = useState<Set<string>>(new Set());
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
      setSharingPeers(new Set());
      setWatching(new Set());
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
    listen<{ pubkey: string; sharing: boolean }>("voice://peer-video-sharing", (e) => {
      setSharingPeers((prev) => {
        const next = new Set(prev);
        if (e.payload.sharing) next.add(e.payload.pubkey); else next.delete(e.payload.pubkey);
        return next;
      });
      // A stopped share should also close any open viewer for that peer.
      if (!e.payload.sharing) {
        setWatching((prev) => {
          if (!prev.has(e.payload.pubkey)) return prev;
          const next = new Set(prev);
          next.delete(e.payload.pubkey);
          return next;
        });
      }
    }).then(safePush);

    return () => { cleanupRan = true; unlisten.forEach((u) => u()); };
  }, [applyState]);

  const refreshDisplaySources = useCallback(async () => {
    try {
      const s = await api.listDisplaySources();
      setDisplaySources(s);
      setSourceId((cur) => (cur && s.some((x) => x.id === cur)) ? cur : (s[0]?.id ?? null));
    } catch { /* ignore */ }
  }, []);

  // User-selectable quality (the share popover sets these). Resolution maps to a
  // max width/height the backend downscales to; "source" = no downscale.
  const [shareFps, setShareFps] = useState(30);
  const [shareResId, setShareResId] = useState<"720" | "1080" | "source">("720");
  const startShare = useCallback(async () => {
    // No catch: a start failure leaves isSharing=false (correct); the rejection
    // surfaces as an unhandledrejection, consistent with toggleTransmit.
    const [mw, mh] = shareResId === "1080" ? [1920, 1080] : shareResId === "source" ? [7680, 4320] : [1280, 720];
    await api.voiceStartScreenShare(shareFps, mw, mh, sourceId, audioDeviceId);
    setIsSharing(true);
  }, [shareFps, shareResId, sourceId, audioDeviceId]);
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

  // sharingPeers only ever contains OTHER peers (backend never emits
  // peer-video-sharing for the local client), so size > 0 means someone else is sharing.
  const someoneElseSharing = sharingPeers.size > 0;

  const toggleWatch = useCallback((pubkey: string) => {
    setWatching((prev) => (prev.has(pubkey) ? new Set() : new Set([pubkey])));
  }, []);
  const setGameAudioVolume = useCallback((pubkey: string, gain: number) => {
    void api.voiceSetScreenAudioGain(pubkey, gain);
  }, []);

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

  return { inCall, muted, deafened, transmitting, localSpeaking, peers, join, leave, setMute, setDeafen, toggleTransmit, connectionQuality, peerVolume, setPeerVolume, isSharing, startShare, stopShare, audioDevices, audioDeviceId, setAudioDeviceId, displaySources, sourceId, setSourceId, someoneElseSharing, sharingPeers, watching, toggleWatch, setGameAudioVolume, refreshDisplaySources, shareFps, setShareFps, shareResId, setShareResId };
}
