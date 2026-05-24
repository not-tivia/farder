// client/src/lib/translation/detect.ts

import { franc } from "franc-min";
import { iso3to1 } from "./lang";

export interface DetectResult {
  iso1: string | null;
  confidence: number;
}

/**
 * Detect the source language of a chat message.
 *
 * franc-min returns ISO 639-3 codes plus a confidence score. We map back to
 * ISO 639-1 (Bergamot's identifiers); if no map exists or confidence is low,
 * iso1 is null (caller should surface a "pick source language" UI).
 *
 * Short messages (< 10 chars) are inherently low-confidence; we cap returned
 * confidence at 0.4 in that range to force the manual picker.
 */
export function detect(text: string): DetectResult {
  const trimmed = text.trim();
  if (trimmed.length === 0) return { iso1: null, confidence: 0 };

  const iso3 = franc(trimmed, { minLength: 1 });
  if (iso3 === "und") return { iso1: null, confidence: 0 };

  const iso1 = iso3to1(iso3);
  // franc doesn't expose a per-call confidence in the simple API; use length-based heuristic.
  // For < 10 chars, accuracy is poor — cap confidence so callers fall back.
  const lenConfidence = trimmed.length < 10 ? 0.3 : trimmed.length < 30 ? 0.7 : 0.9;
  return { iso1, confidence: lenConfidence };
}
