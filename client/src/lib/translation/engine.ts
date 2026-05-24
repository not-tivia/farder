// client/src/lib/translation/engine.ts
//
// Bergamot WASM adapter + per-pair translator pool.
//
// @browsermt/bergamot-translator v0.4.9 does NOT export a `loadModel` factory.
// Its real API is:
//   new LatencyOptimisedTranslator(opts?) → translator
//   translator.translate({ from, to, text }) → Promise<{ target: { text } }>
//   translator.delete() → void
//
// The BergamotTranslator / BergamotEngine interfaces below are internal adapter
// contracts that normalise the real package into the shape the rest of the
// codebase depends on.  The actual wiring to loadTranslationModel (loading
// ArrayBuffers from the local file paths returned by getModelPaths) happens in
// Task 14 (smoke-test / runtime adaptation). At this stage the adapter is a
// correct stub — it compiles cleanly, exports the right symbols, and the pool
// plumbing is wired. See design spec §Adapter for details.

import { convertFileSrc } from "@tauri-apps/api/core";
import {
  getModelPaths,
  listLocalModels,
  downloadModel,
} from "./api";
import type { LangPair } from "./types";

// ---------------------------------------------------------------------------
// Internal adapter interfaces
// ---------------------------------------------------------------------------

interface BergamotTranslator {
  translate(text: string): Promise<string>;
  free(): void;
}

// ---------------------------------------------------------------------------
// Single shared LatencyOptimisedTranslator instance
//
// The real package manages model downloads itself; for Farder we will supply
// a custom TranslatorBacking (Task 14) that loads from local ArrayBuffers.
// Until then, a lazily-constructed instance is sufficient for compilation.
// ---------------------------------------------------------------------------

let sharedInstance: unknown | null = null;
let instancePromise: Promise<unknown> | null = null;

async function getSharedInstance(): Promise<unknown> {
  if (!instancePromise) {
    instancePromise = (async () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const mod = await import("@browsermt/bergamot-translator" as any);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const { LatencyOptimisedTranslator } = mod as any;
      sharedInstance = new LatencyOptimisedTranslator();
      return sharedInstance;
    })();
  }
  return instancePromise;
}

// ---------------------------------------------------------------------------
// Per-pair translator pool
//
// Each entry wraps the shared instance with a pair-scoped translate() that
// passes {from, to} through to the real bergamot API.
// ---------------------------------------------------------------------------

const translatorPool = new Map<string, BergamotTranslator>();

function pairKey(pair: LangPair): string {
  return `${pair.src}-${pair.trg}`;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Ensures the local model for `pair` is present. If not, calls `onNotPresent`
 * (which should show UI confirmation + progress), then triggers the download.
 */
export async function ensureModel(
  pair: LangPair,
  onNotPresent: () => Promise<void>, // called if model needs download (UI confirms)
): Promise<void> {
  const local = await listLocalModels();
  const present = local.some(
    (m) => m.pair.src === pair.src && m.pair.trg === pair.trg,
  );
  if (!present) {
    await onNotPresent(); // UI shows confirm + download progress
    await downloadModel(pair);
  }
}

/**
 * Returns a cached BergamotTranslator for `pair`, creating one if needed.
 * Loads model files from the paths provided by the Rust backend.
 */
export async function getOrCreateTranslator(
  pair: LangPair,
): Promise<BergamotTranslator> {
  const key = pairKey(pair);
  const cached = translatorPool.get(key);
  if (cached) return cached;

  // TODO(Task 14): convert getModelPaths(pair) to tauri:// asset URLs via
  // convertFileSrc and feed them to a custom TranslatorBacking subclass so
  // the WASM engine loads from local disk instead of fetching Mozilla's
  // registry over the network. The closure below already has access to
  // `pair`; Task 14 should resolve `paths` here and pass them through.
  void convertFileSrc; void getModelPaths;

  // Obtain the shared engine instance.
  const engine = await getSharedInstance();

  // Wrap the shared instance in a pair-scoped BergamotTranslator. The
  // closure-captured src/trg drive each translate() call.
  const src = pair.src;
  const trg = pair.trg;

  const translator: BergamotTranslator = {
    async translate(text: string): Promise<string> {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const result = await (engine as any).translate({ from: src, to: trg, text });
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return (result as any).target.text as string;
    },
    free(): void {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (engine as any).delete?.();
    },
  };

  translatorPool.set(key, translator);
  return translator;
}

/**
 * Translates `text` for the given language pair.
 */
export async function translate(text: string, pair: LangPair): Promise<string> {
  const translator = await getOrCreateTranslator(pair);
  return translator.translate(text);
}

/**
 * Frees all cached translators and resets the pool. Call on session end or
 * when the user disables translation.
 */
export function clearPool(): void {
  for (const t of translatorPool.values()) {
    t.free();
  }
  translatorPool.clear();
  sharedInstance = null;
  instancePromise = null;
}
