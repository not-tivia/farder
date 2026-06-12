import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

export interface InvitePreview {
  status: "loading" | "ok" | "invalid" | "unavailable" | "none";
  serverName: string | null;
  memberCount: number | null;
  onlineCount: number | null;
}

const NONE: InvitePreview = { status: "none", serverName: null, memberCount: null, onlineCount: null };

// Session cache by link with ~60s TTL (matches the Rust relay cache TTL).
// Entries older than 60s are treated as misses so preview data stays fresh.
const cache = new Map<string, { at: number; value: InvitePreview }>();
const pending = new Map<string, Promise<InvitePreview>>();

export function useInvitePreview(link?: string | null): InvitePreview {
  const [preview, setPreview] = useState<InvitePreview>(() => {
    if (!link) return NONE;
    const e = cache.get(link);
    const hit = e && Date.now() - e.at < 60_000 ? e.value : undefined;
    return hit ?? { ...NONE, status: "loading" };
  });

  useEffect(() => {
    if (!link) { setPreview(NONE); return; }
    const e = cache.get(link);
    const hit = e && Date.now() - e.at < 60_000 ? e.value : undefined;
    if (hit) { setPreview(hit); return; }
    setPreview({ ...NONE, status: "loading" });
    let cancelled = false;
    let p = pending.get(link);
    if (!p) {
      p = api.getInvitePreview(link)
        .then((v): InvitePreview => {
          const result: InvitePreview = {
            status: v.status,
            serverName: v.server_name ?? null,
            memberCount: v.member_count ?? null,
            onlineCount: v.online_count ?? null,
          };
          // Don't pin transient failures — allow a retry on the next mount.
          // Also skip "unavailable" so the TTL never pins a transient outage.
          if (v.status !== "unavailable") cache.set(link, { at: Date.now(), value: result });
          pending.delete(link);
          return result;
        })
        .catch((): InvitePreview => { pending.delete(link); return { ...NONE, status: "unavailable" }; });
      pending.set(link, p);
    }
    p.then((r) => { if (!cancelled) setPreview(r); });
    return () => { cancelled = true; };
  }, [link]);

  return preview;
}
