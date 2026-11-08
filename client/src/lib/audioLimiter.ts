// The loudness limiter for sound that plays in the web view: videos and voice
// messages in chat. Voice calls and shared-screen audio are limited in the Rust
// mixer instead (src-tauri/src/audio_limit.rs); both read the same two settings,
// the master volume and the max loudness, so there is one control for everything.
//
// Safe by default: while both settings are at their defaults (100%, no ceiling)
// no element is touched at all, so ordinary playback cannot be affected by this
// code. When they are not, each element is routed through the worklet.
//
// What it cannot reach: sound inside someone else's page (the YouTube/Spotify
// players are iframes from another origin; a page cannot process another page's
// audio). Those play at their own volume.

import * as api from "./tauri-bridge";
import { LIMITER_PROCESSOR_NAME, LIMITER_WORKLET_SOURCE } from "./audioLimiterWorklet";

export interface AudioLimits {
  /** 0..1.5, 1 = unchanged. */
  master: number;
  /** Ceiling in dB (-40..-3); 0 = no ceiling. */
  maxDb: number;
}

export const AUDIO_LIMITS_CHANGED = "farder:audio-limits-changed";
const DEFAULTS: AudioLimits = { master: 1, maxDb: 0 };

let limits: AudioLimits = { ...DEFAULTS };

/** Whether any element needs to be routed through the limiter at all. */
export function limiterNeeded(l: AudioLimits): boolean {
  return l.master !== 1 || l.maxDb < 0;
}

export function dbToLinear(db: number): number {
  return Math.pow(10, db / 20);
}

/** The worklet's message for these limits (ceiling as a linear level, 0 = off). */
export function workletMessage(l: AudioLimits): { master: number; ceiling: number } {
  return { master: l.master, ceiling: l.maxDb < 0 ? dbToLinear(l.maxDb) : 0 };
}

interface Graph {
  ctx: AudioContext;
  node: AudioWorkletNode;
}
let graphPromise: Promise<Graph | null> | null = null;
let graph: Graph | null = null;

/** Build the audio graph once. Resolves null (and never throws) if it cannot. */
function ensureGraph(): Promise<Graph | null> {
  if (graphPromise) return graphPromise;
  graphPromise = (async () => {
    try {
      const ctx = new AudioContext();
      const url = URL.createObjectURL(new Blob([LIMITER_WORKLET_SOURCE], { type: "application/javascript" }));
      try { await ctx.audioWorklet.addModule(url); } finally { URL.revokeObjectURL(url); }
      const node = new AudioWorkletNode(ctx, LIMITER_PROCESSOR_NAME, {
        numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [2],
        channelCount: 2, channelCountMode: "explicit",
      });
      node.connect(ctx.destination);
      node.port.postMessage(workletMessage(limits));
      graph = { ctx, node };
      return graph;
    } catch (e) {
      console.warn("[audio] the loudness limiter could not start; sound plays unprocessed:", e);
      return null;
    }
  })();
  return graphPromise;
}

const attached = new WeakSet<HTMLMediaElement>();

/**
 * Route this element through the limiter, if the person has asked for one.
 * Idempotent, and a no-op at the default settings. If the graph is not ready yet
 * the element is held muted for the instant it takes, so nothing can blast out
 * unprocessed in the gap.
 */
export function attachLimiter(el: HTMLMediaElement | null): void {
  if (!el || attached.has(el) || !limiterNeeded(limits)) return;
  attached.add(el);
  const wasMuted = el.muted;
  if (!graph) el.muted = true;
  void ensureGraph().then((g) => {
    if (!g) { el.muted = wasMuted; return; } // could not start: leave the element alone
    try {
      g.ctx.createMediaElementSource(el).connect(g.node);
      void g.ctx.resume();
      el.addEventListener("play", () => { void g.ctx.resume(); });
    } catch (e) {
      console.warn("[audio] could not route an element through the limiter:", e);
    }
    el.muted = wasMuted;
  });
}

/** Take new limits (from the settings screen or startup). */
export function applyAudioLimits(l: AudioLimits): void {
  limits = { master: l.master, maxDb: l.maxDb };
  if (graph) graph.node.port.postMessage(workletMessage(limits));
  else if (limiterNeeded(limits)) void ensureGraph(); // warm it up so the first play is not held
}

/** Read the saved limits once at startup and keep in step with the settings screen. */
export async function initAudioLimits(): Promise<void> {
  try {
    const l = await api.getAudioLimits();
    applyAudioLimits({ master: l.master, maxDb: l.max_db });
  } catch { /* defaults: unprocessed */ }
  window.addEventListener(AUDIO_LIMITS_CHANGED, (e) => {
    const d = (e as CustomEvent<AudioLimits>).detail;
    if (d) applyAudioLimits(d);
  });
}
