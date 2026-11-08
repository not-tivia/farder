// The sender's own garbler choice, stored on this device only. Nothing here is
// sent to a server: the garbled words are all anyone else ever receives.

import { MODES, type GarbleMode, sanitizePhrase } from "./garble";

export interface GarbleSettings {
  enabled: boolean;
  mode: GarbleMode;
  phrase: string; // used by mode "custom"
}

const KEY = "farder.garble";
export const GARBLE_CHANGED = "farder:garble-changed";

export const DEFAULT_GARBLE: GarbleSettings = { enabled: false, mode: "bark", phrase: "" };

export function getGarble(): GarbleSettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_GARBLE;
    const v = JSON.parse(raw) as Partial<GarbleSettings>;
    const mode = MODES.includes(v.mode as GarbleMode) ? (v.mode as GarbleMode) : "bark";
    return {
      enabled: v.enabled === true,
      mode,
      phrase: sanitizePhrase(typeof v.phrase === "string" ? v.phrase : ""),
    };
  } catch {
    return DEFAULT_GARBLE; // storage blocked or corrupt: behave as "off"
  }
}

export function setGarble(next: GarbleSettings): void {
  try {
    localStorage.setItem(KEY, JSON.stringify({ ...next, phrase: sanitizePhrase(next.phrase) }));
  } catch {
    /* storage unavailable: the change simply does not persist */
  }
  window.dispatchEvent(new CustomEvent(GARBLE_CHANGED));
}
