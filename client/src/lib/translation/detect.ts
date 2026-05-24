// client/src/lib/translation/detect.ts

import { francAll } from "franc-min";
import { iso3to1 } from "./lang";

export interface DetectResult {
  iso1: string | null;
  confidence: number;
}

/**
 * Detect the source language of a chat message.
 *
 * Uses franc-min's `francAll`, which returns `[iso3, score][]` sorted by
 * descending score (0-1, 1 = perfect match). Decisiveness comes from BOTH
 * the top match's raw score AND its lead over the second-best match —
 * short or rare-word strings often score every language similarly.
 *
 * We deliberately let franc apply its default 10-character `minLength`
 * floor (so 1-9 char input returns `und` straight away). For 10-29 char
 * input we require a tighter score AND a clear lead; for ≥30 char we
 * loosen because trigram detection becomes reliable on longer text.
 *
 * The returned `confidence` is interpreted by the store as: ≥0.5 means
 * "use this language", <0.5 means "ask the user to pick".
 */
export function detect(text: string): DetectResult {
  const trimmed = text.trim();
  if (trimmed.length === 0) return { iso1: null, confidence: 0 };

  // franc default minLength=10 returns [["und", 1]] for shorter input.
  const all = francAll(trimmed);
  const top = all[0];
  if (!top || top[0] === "und") {
    return { iso1: null, confidence: 0 };
  }

  const [topIso3, topScore] = top;
  const second = all[1];
  const secondScore = second ? second[1] : 0;
  const lead = topScore - secondScore;

  const iso1 = iso3to1(topIso3);
  if (!iso1) {
    // Detected a language we don't have an ISO-1 mapping for; treat as
    // unknown rather than guess.
    return { iso1: null, confidence: 0 };
  }

  // Decisiveness thresholds calibrated to keep false positives low.
  // Short text (10-29 chars): franc is often confused between similar
  // Romance/Germanic languages; demand both a high score AND a clear lead.
  // Longer text (≥30 chars): the trigram model is reliable enough that we
  // accept anything with a decent score.
  const len = trimmed.length;
  let confidence: number;
  if (len < 30) {
    if (topScore >= 0.95 && lead >= 0.03) {
      confidence = 0.7;
    } else {
      confidence = 0.3; // surface low-confidence picker
    }
  } else {
    if (topScore >= 0.9) confidence = 0.9;
    else if (topScore >= 0.8) confidence = 0.6;
    else confidence = 0.3;
  }

  return { iso1, confidence };
}
