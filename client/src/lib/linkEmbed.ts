// Mirrors farder_protocol::messages embed types (serde external tagging).
export type EmbedKind = "Tweet" | "Video" | "Image" | "Audio" | "Article";

export interface EmbedMedia {
  url: string;
  mime: string;
  width: number | null;
  height: number | null;
  playable_inline: boolean;
}

export interface LinkEmbed {
  provider: string;
  kind: EmbedKind;
  url: string;
  title: string | null;
  author: string | null;
  description: string | null;
  thumbnail: string | null;
  media: EmbedMedia | null;
  duration_secs: number | null;
}

// serde serializes `EmbedOutcome::Embed(x)` as { Embed: x }, and the unit
// variants as the bare strings "Unsupported" / "Unavailable".
export type EmbedOutcome = { Embed: LinkEmbed } | "Unsupported" | "Unavailable";

// Allowlist host detection — MUST match the relay's ALLOWLIST (embed.rs).
// User-facing hosts only: fxtwitter.com/api.fxtwitter.com are internal API
// hosts used by the relay, never posted by users, so they are excluded.
// Direct-image hosts (i.imgur.com, etc.) are intentionally NOT here: Farder
// already inlines posted image URLs via the auto-attach path (MessageInput ->
// fetch_url), so an image embed would render the picture twice.
const ALLOWLIST_HOSTS = [
  "twitter.com", "x.com", "youtube.com", "youtu.be",
  "reddit.com", "redd.it", "open.spotify.com",
];

function hostAllowed(host: string): boolean {
  const h = host.toLowerCase();
  return ALLOWLIST_HOSTS.some((s) => h === s || h.endsWith("." + s));
}

const URL_RE = /https?:\/\/[^\s<>"']+/g;

/**
 * Extract up to 3 unique allowlisted URLs from message text.
 *
 * Test-notes (manually verified by inspection):
 *   - "check out https://x.com/a/status/1"  → ["https://x.com/a/status/1"]
 *   - "https://evil.com/x"                  → []  (not allowlisted)
 *   - "https://youtube.com.evil.com/x"      → []  (subdomain trick rejected)
 *   - "https://www.youtube.com/watch?v=abc" → ["https://www.youtube.com/watch?v=abc"]
 *     (www.youtube.com ends with ".youtube.com")
 *   - "https://open.spotify.com/track/1"    → ["https://open.spotify.com/track/1"]
 *     (open.spotify.com ends with ".spotify.com"? No — exact match on "open.spotify.com")
 *   - "https://i.imgur.com/pic.jpg"         → []  (image hosts excluded; auto-attach handles images)
 *   - same URL twice                        → deduplicated to 1 entry
 *   - 4 distinct URLs                       → first 3 only (cap enforced)
 *   - not-a-url embedded text               → skipped (new URL() throws, caught)
 */
export function detectEmbedUrls(text: string): string[] {
  const found: string[] = [];
  const seen = new Set<string>();
  for (const m of text.match(URL_RE) ?? []) {
    if (found.length >= 3) break;
    let host: string;
    try { host = new URL(m).host; } catch { continue; }
    if (!hostAllowed(host)) continue;
    if (seen.has(m)) continue;
    seen.add(m);
    found.push(m);
  }
  return found;
}
