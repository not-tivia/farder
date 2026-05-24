// client/src/lib/translation/store.ts

import { detect } from "./detect";
import { ensureModel, translate } from "./engine";
import { getTranslationSettings } from "./api";
import type { TranslationStatus } from "./types";

type Listener = (state: Map<string, TranslationStatus>) => void;

const state = new Map<string, TranslationStatus>();
const listeners = new Set<Listener>();

function emit(): void {
  const snapshot = new Map(state);
  for (const l of listeners) l(snapshot);
}

export function subscribe(l: Listener): () => void {
  listeners.add(l);
  l(new Map(state));
  return () => { listeners.delete(l); };
}

export function getStatus(messageId: string): TranslationStatus {
  return state.get(messageId) ?? { kind: "idle" };
}

export function dismiss(messageId: string): void {
  state.delete(messageId);
  emit();
}

export interface TranslateOptions {
  messageId: string;
  content: string;
  defaultTarget: string;
  /** Hex public-key of the message author. When present, the store looks up
   *  per-user language overrides before running detection. Pass empty/omit to
   *  bypass the override lookup. */
  authorPublicKeyHex?: string;
  /**
   * Called when a model needs to be downloaded. The UI must show a privacy
   * disclosure dialog and either resolve (user accepted) or throw (user cancelled).
   */
  confirmDownload: (pair: { src: string; trg: string }) => Promise<void>;
}

export async function translateMessage(opts: TranslateOptions): Promise<void> {
  const { messageId, content, defaultTarget, confirmDownload, authorPublicKeyHex } = opts;

  // Idempotency guard — auto-translate (Message.tsx useEffect) can re-fire
  // this on re-render. If we've already kicked off / finished translation
  // for this message, just return. Only the "idle" implicit state OR an
  // explicit "error" should retry.
  const existing = state.get(messageId);
  if (existing && existing.kind !== "error") {
    return;
  }

  // Check per-user language override first — if present, skip detection and
  // route directly through translateMessageWithSource. A blank string is
  // treated as "no override lookup".
  if (authorPublicKeyHex) {
    try {
      const settings = await getTranslationSettings();
      const override = settings.user_language_overrides[authorPublicKeyHex];
      if (override && override !== defaultTarget) {
        return translateMessageWithSource({ ...opts, src: override });
      }
      if (override && override === defaultTarget) {
        state.set(messageId, { kind: "already-in-target", lang: defaultTarget });
        emit();
        return;
      }
    } catch (e) {
      // If settings fetch fails, fall through to detection — don't block the
      // user on a corrupt settings file.
      // eslint-disable-next-line no-console
      console.error("[translate] override lookup failed:", e);
    }
  }

  state.set(messageId, { kind: "detecting" });
  emit();

  const det = detect(content);
  if (det.iso1 === null || det.confidence < 0.5) {
    state.set(messageId, { kind: "low-confidence", suggested: det.iso1 ?? undefined });
    emit();
    return;
  }
  if (det.iso1 === defaultTarget) {
    state.set(messageId, { kind: "already-in-target", lang: defaultTarget });
    emit();
    return;
  }

  const pair = { src: det.iso1, trg: defaultTarget };
  state.set(messageId, { kind: "loading-model", src: pair.src, trg: pair.trg });
  emit();

  try {
    await ensureModel(pair, async () => {
      await confirmDownload(pair);
      // Note: actual progress events are emitted by the Rust side via `translation:progress`
      // and consumed by the UI. The store doesn't subscribe to them directly; the
      // download dialog component does and updates its own progress UI.
    });
    state.set(messageId, { kind: "translating", src: pair.src, trg: pair.trg });
    emit();
    const text = await translate(content, pair);
    state.set(messageId, { kind: "done", text, src: pair.src, trg: pair.trg });
    emit();
  } catch (err) {
    state.set(messageId, {
      kind: "error",
      reason: err instanceof Error ? err.message : String(err),
    });
    emit();
  }
}

/** Called when picking a source language manually (low-confidence path). */
export async function translateMessageWithSource(
  opts: TranslateOptions & { src: string },
): Promise<void> {
  const { messageId, content, src, defaultTarget, confirmDownload } = opts;
  if (src === defaultTarget) {
    state.set(messageId, { kind: "already-in-target", lang: defaultTarget });
    emit();
    return;
  }
  const pair = { src, trg: defaultTarget };
  state.set(messageId, { kind: "loading-model", src, trg: defaultTarget });
  emit();
  try {
    await ensureModel(pair, async () => { await confirmDownload(pair); });
    state.set(messageId, { kind: "translating", src, trg: defaultTarget });
    emit();
    const text = await translate(content, pair);
    state.set(messageId, { kind: "done", text, src, trg: defaultTarget });
    emit();
  } catch (err) {
    state.set(messageId, {
      kind: "error",
      reason: err instanceof Error ? err.message : String(err),
    });
    emit();
  }
}
