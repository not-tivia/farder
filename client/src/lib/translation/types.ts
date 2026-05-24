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
  vocab: string;
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
