// Pure connection-quality tier classifier for the voice control bar.
// Single source of truth for the UI thresholds (see voice-polish design,
// Feature 3). Dependency-free and side-effect-free so its logic is trivially
// correct by inspection.

export type QualityTier = "good" | "fair" | "poor";

/**
 * Classify a QUIC link sample into a display tier.
 *
 *   good  when rttMs < 100 && lossPct < 2
 *   fair  when (not good) and (rttMs < 250 || lossPct < 8)
 *   poor  otherwise
 */
export function classifyQuality(rttMs: number, lossPct: number): QualityTier {
  if (rttMs < 100 && lossPct < 2) return "good";
  if (rttMs < 250 || lossPct < 8) return "fair";
  return "poor";
}
