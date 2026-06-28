export interface ParsedInvite {
  address?: string;
  inviteCode?: string;
  setupToken?: string;
}

// Decode URL-safe base64 (no-pad) used by farder.gg/join links.
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

  // farder.gg/join/ENCODED
  const joinMatch = trimmed.match(/(?:https?:\/\/)?farder\.gg\/join\/([A-Za-z0-9_-]+)/);
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
