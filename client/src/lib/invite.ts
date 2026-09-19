export interface ParsedInvite {
  address?: string;
  inviteCode?: string;
  setupToken?: string;
}

/** The host that serves web invite links. Mirrors `WEB_INVITE_HOST` in
 *  `client/src-tauri/src/connection.rs` — the two cannot share a constant
 *  across the language boundary, so a domain move means editing both. */
export const WEB_INVITE_HOST = "farder.xyz";

/** `WEB_INVITE_HOST` as a regex-safe fragment (a TLD dot must not match any
 *  character, or `farderaxyz/join/...` would parse as ours). */
const HOST_PATTERN = WEB_INVITE_HOST.replace(/\./g, "\\.");

// Decode URL-safe base64 (no-pad) used by <host>/join links.
function b64urlDecode(s: string): string {
  let t = s.replace(/-/g, "+").replace(/_/g, "/");
  while (t.length % 4) t += "=";
  return atob(t);
}

/**
 * Parse a Farder invite (a pasted link or a farder:// deep link) into a
 * connection target. Relay-aware: a relay deep link (farder://relay/...) is
 * returned whole as `address` (connect_server parses it; the invite token is
 * embedded). Direct links return `address` + `inviteCode`/`setupToken`.
 *
 * Extra fallbacks (preserved from original components):
 *   - 64-char hex string → standalone setupToken
 *   - host:port (no path) → address only
 *   - anything else → inviteCode (short code used with a saved server address)
 */
export function parseInviteLink(input: string): ParsedInvite {
  const trimmed = input.trim();
  if (!trimmed) return {};

  // <WEB_INVITE_HOST>/join/ENCODED
  //
  // Anchored to the start or a space so the host cannot be borrowed as a PATH:
  // the old pattern matched anywhere, so `https://evil.com/farder.xyz/join/X`
  // parsed as one of ours. Still tolerant of a link pasted inside a sentence,
  // which is how invites actually arrive.
  const joinMatch = trimmed.match(
    new RegExp(`(?:^|\\s)(?:https?://)?${HOST_PATTERN}/join/([A-Za-z0-9_-]+)`),
  );
  if (joinMatch) {
    try {
      const decoded = b64urlDecode(joinMatch[1]);
      if (decoded.startsWith("farder://")) {
        return parseInviteLink(decoded);
      }
      const slashIdx = decoded.indexOf("/");
      if (slashIdx > 0) {
        const address = decoded.substring(0, slashIdx);
        const token = decoded.substring(slashIdx + 1);
        if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
        return { address, inviteCode: token };
      }
    } catch {}
    return {};
  }

  // Relay deep link (full or compact default-relay form): return the whole URL
  // as the address (the Rust connect parser needs it intact for the handshake),
  // BUT also surface the embedded invite code so the mesh join (join_log_server /
  // ResolveInvite) can use the bare code. Segment positions mirror the Rust
  // parse_relay_target: compact `farder://relayd/<server_id>/<code>` → code is
  // index 1; full `farder://relay/<addr>/<server_id>/<cert_fp>/<code>` → index 3.
  if (/^farder:\/\/relayd\//i.test(trimmed)) {
    const parts = trimmed.replace(/^farder:\/\/relayd\//i, "").split("/");
    return { address: trimmed, inviteCode: parts[1] || undefined };
  }
  if (/^farder:\/\/relay\//i.test(trimmed)) {
    const parts = trimmed.replace(/^farder:\/\/relay\//i, "").split("/");
    return { address: trimmed, inviteCode: parts[3] || undefined };
  }

  // Direct farder://addr/code
  const farderMatch = trimmed.match(/^farder:\/\/([^/]+)\/(.+)$/i);
  if (farderMatch) {
    const address = farderMatch[1];
    const token = farderMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  // host:port/code (no scheme)
  const slashMatch = trimmed.match(/^([^/]+:\d+)\/(.+)$/);
  if (slashMatch) {
    const address = slashMatch[1];
    const token = slashMatch[2];
    if (token.startsWith("setup:")) return { address, setupToken: token.slice(6) };
    return { address, inviteCode: token };
  }

  // 64-char hex = standalone setup token (used with a saved server address)
  if (/^[0-9a-f]{64}$/i.test(trimmed)) {
    return { setupToken: trimmed };
  }

  // host:port only (no invite)
  if (/^.+:\d+$/.test(trimmed)) {
    return { address: trimmed };
  }

  // Short string = invite code (used with a saved server address)
  return { inviteCode: trimmed };
}
