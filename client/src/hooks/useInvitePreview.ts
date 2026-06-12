import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

export interface InvitePreview {
  status: "loading" | "ok" | "invalid" | "unavailable" | "none";
  serverName: string | null;
  memberCount: number | null;
  onlineCount: number | null;
}

const NONE: InvitePreview = { status: "none", serverName: null, memberCount: null, onlineCount: null };

// Session cache by link. The Rust side has its own 60s TTL cache; this one just
// prevents re-invoking per render/mount.
const cache = new Map<string, InvitePreview>();
const pending = new Map<string, Promise<InvitePreview>>();

export function useInvitePreview(link?: string | null): InvitePreview {
  const [preview, setPreview] = useState<InvitePreview>(
    link ? cache.get(link) ?? { ...NONE, status: "loading" } : NONE,
  );

  useEffect(() => {
    if (!link) { setPreview(NONE); return; }
    const hit = cache.get(link);
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
          // Don't pin transient failures for the whole session — allow a
          // retry on the next mount.
          if (v.status !== "unavailable") cache.set(link, result);
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
