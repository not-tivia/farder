// client/src/lib/translation/types.ts

export interface LangPair {
  src: string;
  trg: string;
}

export interface LocalModel {
  pair: LangPair;
  disk_size_bytes: number;
  downloaded_at: number;
  version: string;
}

export interface ModelPaths {
  model: string;
  /** Single shared vocab. Present when the model uses one vocabulary for both
   *  source and target. Mutually exclusive with src_vocab/trg_vocab. */
  vocab?: string;
  /** Source vocab for split-vocab models (en-zh, en-ja, en-ko, etc.). */
  src_vocab?: string;
  /** Target vocab for split-vocab models. */
  trg_vocab?: string;
  lex: string;
}

export interface AvailablePair {
  src: string;
  trg: string;
  size_bytes: number;
  display_name: string;
}

export interface TranslationSettings {
  enabled: boolean;
  default_target: string;
  seen_first_run: boolean;
}

export interface DownloadProgress {
  pair: LangPair;
  bytes_done: number;
  bytes_total: number;
  stage: "downloading" | "decompressing" | "verifying" | "saving" | "done" | "error";
  message?: string | null;
}

export type TranslationStatus =
  | { kind: "idle" }
  | { kind: "detecting" }
  | { kind: "loading-model"; src: string; trg: string }
  | { kind: "downloading-model"; src: string; trg: string; progress: DownloadProgress }
  | { kind: "translating"; src: string; trg: string }
  | { kind: "done"; text: string; src: string; trg: string }
  | { kind: "already-in-target"; lang: string }
  | { kind: "low-confidence"; suggested?: string }
  | { kind: "error"; reason: string };
