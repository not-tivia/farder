// Opt-in in-app players for YouTube/Spotify embeds. Pure helpers + a tiny
// localStorage-backed consent store. No React, no Tauri — keeps the feature
// seam-free (see CLAUDE.md on the untyped Tauri seam). Consent defaults to
// DENIED and fails closed on any storage error (privacy-safe).

export type EmbedProvider = "youtube" | "spotify";

const CONSENT_KEY: Record<EmbedProvider, string> = {
  youtube: "farder.embedConsent.youtube",
  spotify: "farder.embedConsent.spotify",
};

export function providerLabel(p: EmbedProvider): string {
  return p === "youtube" ? "YouTube" : "Spotify";
}

function ytSrc(id: string): string {
  return `https://www.youtube-nocookie.com/embed/${encodeURIComponent(id)}`;
}

/**
 * Detect the provider from the URL host and build the official embed-player src,
 * or return null if the host is unsupported or the id can't be parsed.
 *
 * Test-notes (verified by inspection):
 *   - "https://youtu.be/abc123"                      -> {youtube, https://www.youtube-nocookie.com/embed/abc123}
 *   - "https://www.youtube.com/watch?v=abc123&t=10"  -> {youtube, .../embed/abc123}
 *   - "https://youtube.com/shorts/abc123"            -> {youtube, .../embed/abc123}
 *   - "https://open.spotify.com/track/xyz789?si=1"   -> {spotify, https://open.spotify.com/embed/track/xyz789}
 *   - "https://open.spotify.com/intl-de/album/xyz"   -> {spotify, .../embed/album/xyz}  (locale prefix tolerated)
 *   - "https://example.com/x"                         -> null  (unsupported host)
 *   - "not a url"                                      -> null  (URL ctor throws, caught)
 *   - "https://www.youtube.com/feed/subscriptions"    -> null  (no video id)
 */
export function buildEmbedPlayerSrc(url: string): { provider: EmbedProvider; src: string } | null {
  let u: URL;
  try { u = new URL(url); } catch { return null; }
  const host = u.host.toLowerCase();

  if (host === "youtu.be") {
    const id = u.pathname.split("/").filter(Boolean)[0];
    return id ? { provider: "youtube", src: ytSrc(id) } : null;
  }
  if (host === "youtube.com" || host === "www.youtube.com" || host === "m.youtube.com") {
    const v = u.searchParams.get("v");
    if (v) return { provider: "youtube", src: ytSrc(v) };
    const parts = u.pathname.split("/").filter(Boolean); // ["shorts","abc"] | ["embed","abc"]
    if ((parts[0] === "shorts" || parts[0] === "embed") && parts[1]) {
      return { provider: "youtube", src: ytSrc(parts[1]) };
    }
    return null;
  }

  if (host === "open.spotify.com") {
    const parts = u.pathname.split("/").filter(Boolean); // ["track","xyz"] | ["intl-de","track","xyz"]
    const allowed = ["track", "album", "playlist", "episode", "show"];
    const ti = parts.findIndex((s) => allowed.includes(s));
    if (ti >= 0 && parts[ti + 1]) {
      return { provider: "spotify", src: `https://open.spotify.com/embed/${parts[ti]}/${encodeURIComponent(parts[ti + 1])}` };
    }
    return null;
  }

  return null;
}

/** Read per-provider consent. Fails CLOSED (returns false) on any error. */
export function getEmbedConsent(p: EmbedProvider): boolean {
  try { return localStorage.getItem(CONSENT_KEY[p]) === "1"; } catch { return false; }
}

/** Persist per-provider consent. Swallows storage errors. */
export function setEmbedConsent(p: EmbedProvider, allowed: boolean): void {
  try {
    if (allowed) localStorage.setItem(CONSENT_KEY[p], "1");
    else localStorage.removeItem(CONSENT_KEY[p]);
  } catch { /* ignore */ }
}
