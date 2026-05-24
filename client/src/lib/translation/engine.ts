// client/src/lib/translation/engine.ts
//
// Bergamot WASM adapter + local-file TranslatorBacking.
//
// We subclass @browsermt/bergamot-translator's TranslatorBacking so the
// engine reads model files from ~/.farder/translation-models/<pair>/ (via
// the Rust `get_model_paths` command + Tauri's asset:// protocol) instead
// of fetching from Bergamot's default GCS bucket. No model bytes ever
// leave the device, and a translate() call won't trigger any outbound
// network request once the model is on disk.
//
// One LatencyOptimisedTranslator instance is shared across all pairs;
// Bergamot's worker queues requests and loads each pair's model lazily.

import { convertFileSrc } from "@tauri-apps/api/core";
import {
  getModelPaths,
  listLocalModels,
  downloadModel,
} from "./api";
import type { LangPair } from "./types";

// ---------------------------------------------------------------------------
// Bergamot package shape
//
// The package ships without TypeScript declarations, so we declare a minimal
// surface we depend on. Anything else we touch goes through `any` casts.
// ---------------------------------------------------------------------------

interface BergamotResponse {
  target: { text: string };
}

interface LatencyTranslatorLike {
  translate(req: { from: string; to: string; text: string }): Promise<BergamotResponse>;
  delete(): Promise<void>;
}

// ---------------------------------------------------------------------------
// Lazy single-instance translator
// ---------------------------------------------------------------------------

let translatorPromise: Promise<LatencyTranslatorLike> | null = null;

async function getTranslator(): Promise<LatencyTranslatorLike> {
  if (!translatorPromise) {
    translatorPromise = (async () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const bergamot: any = await import(
        "@browsermt/bergamot-translator/translator.js"
      );
      const LatencyOptimisedTranslator = bergamot.LatencyOptimisedTranslator;
      const TranslatorBacking = bergamot.TranslatorBacking;

      class FarderBacking extends TranslatorBacking {
        constructor() {
          // pivotLanguage: null disables pivoting (v1 supports direct pairs only).
          // registryUrl is unused once loadModelRegistery is overridden, but we
          // set it to a value that will fail fast if the parent path is ever hit.
          super({ pivotLanguage: null, registryUrl: "about:blank" });
        }

        // Bergamot calls this once at construction to populate this.registry.
        // We return the list of locally-downloaded pairs in the minimal shape
        // findModels() needs (only `from` / `to` are read).
        async loadModelRegistery() {
          const local = await listLocalModels();
          return local.map((m) => ({
            from: m.pair.src,
            to: m.pair.trg,
            files: {},
          }));
        }

        // Bergamot calls this per pair, lazily, when translate() needs the
        // model for that pair. We resolve the local file paths via Tauri's
        // asset protocol and fetch the ArrayBuffers in parallel.
        async loadTranslationModel(opts: { from: string; to: string }) {
          const paths = await getModelPaths({ src: opts.from, trg: opts.to });
          const fetchAB = (p: string) =>
            fetch(convertFileSrc(p)).then((r) => r.arrayBuffer());
          const [model, vocab, lex] = await Promise.all([
            fetchAB(paths.model),
            fetchAB(paths.vocab),
            fetchAB(paths.lex),
          ]);
          return {
            model,
            shortlist: lex,
            vocabs: [vocab],
            qualityModel: null,
            config: {},
          };
        }

        // The upstream loadWorker constructs the worker via
        //   `new Worker(new URL('./worker/translator-worker.js', import.meta.url))`
        // which doesn't reliably resolve after Vite bundles a node_modules
        // package. vite.config.ts copies the worker triple into /bergamot/;
        // here we override loadWorker so the Worker is constructed from that
        // known absolute path. The rest of the message-passing setup is the
        // same shape as upstream's loadWorker (translator.js around L117).
        async loadWorker() {
          const worker = new Worker("/bergamot/translator-worker.js");
          let serial = 0;
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const pending = new Map<number, { accept: (v: any) => void; reject: (e: Error) => void; callsite: { message: string; stack?: string } }>();
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const call = (name: string, ...args: any[]) =>
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            new Promise<any>((accept, reject) => {
              const id = ++serial;
              pending.set(id, {
                accept,
                reject,
                callsite: {
                  message: `${name}(${args.map(String).join(", ")})`,
                  stack: new Error().stack,
                },
              });
              worker.postMessage({ id, name, args });
            });
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          worker.addEventListener("message", (evt: MessageEvent<any>) => {
            const { id, result, error } = evt.data;
            const entry = pending.get(id);
            if (!entry) return;
            pending.delete(id);
            if (error !== undefined) {
              const err = Object.assign(new Error(), error, {
                message: `${error.message} (response to ${entry.callsite.message})`,
                stack: error.stack
                  ? `${error.stack}\n${entry.callsite.stack ?? ""}`
                  : entry.callsite.stack,
              });
              entry.reject(err);
            } else {
              entry.accept(result);
            }
          });
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          worker.addEventListener("error", (this as any).onerror.bind(this));
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          await call("initialize", (this as any).options);
          return {
            worker,
            exports: new Proxy(
              {},
              {
                get(_target, name) {
                  if (name !== "then") {
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    return (...args: any[]) => call(name as string, ...args);
                  }
                },
              },
            ),
          };
        }
      }

      return new LatencyOptimisedTranslator(
        { pivotLanguage: null },
        new FarderBacking(),
      ) as LatencyTranslatorLike;
    })();
  }
  return translatorPromise;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Ensures the local model for `pair` is on disk. If not, calls `onNotPresent`
 * (which should show UI confirmation), triggers the Rust download, then tears
 * down the shared translator so the next translate() call rebuilds the
 * backing with a registry that includes the new pair.
 */
export async function ensureModel(
  pair: LangPair,
  onNotPresent: () => Promise<void>,
): Promise<void> {
  const local = await listLocalModels();
  const present = local.some(
    (m) => m.pair.src === pair.src && m.pair.trg === pair.trg,
  );
  if (!present) {
    await onNotPresent();
    await downloadModel(pair);
    // Invalidate the cached translator so the next translate() rebuilds the
    // backing with a registry that now includes the new pair. Tearing down
    // is simpler than poking at private caches and only costs one worker
    // re-spawn per fresh download.
    await clearPool();
  }
}

/**
 * Translates `text` between `pair.src` and `pair.trg`. The shared translator
 * loads the pair's model lazily on first use.
 */
export async function translate(
  text: string,
  pair: LangPair,
): Promise<string> {
  const translator = await getTranslator();
  const result = await translator.translate({
    from: pair.src,
    to: pair.trg,
    text,
  });
  return result.target.text;
}

/**
 * Tears down the shared translator + worker. Next translate() call will
 * lazily re-instantiate. Call on translation-disable or after installing a
 * new model so the registry is re-read.
 */
export async function clearPool(): Promise<void> {
  if (translatorPromise) {
    const t = await translatorPromise.catch(() => null);
    translatorPromise = null;
    if (t) {
      try {
        await t.delete();
      } catch {
        // delete() can throw if the worker never finished booting; safe to
        // swallow — the worker reference is gone either way.
      }
    }
  }
}
