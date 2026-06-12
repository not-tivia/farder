import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

export interface MemberProfile {
  avatarUrl: string | null;
  status: string | null;
}

const EMPTY: MemberProfile = { avatarUrl: null, status: null };

// Keyed by pk:hash — hash alone would let a lying server repoint one member's
// entry at another's cached profile (mirrors the Rust-side key-binding check).
const cache = new Map<string, MemberProfile>();
const pending = new Map<string, Promise<MemberProfile>>();

export function useMemberProfile(
  serverId: string,
  publicKey: string,
  profileHash?: string | null,
): MemberProfile {
  const key = profileHash ? `${publicKey}:${profileHash}` : null;

  const [profile, setProfile] = useState<MemberProfile>(
    key ? cache.get(key) ?? EMPTY : EMPTY,
  );

  useEffect(() => {
    if (!key) { setProfile(EMPTY); return; }
    const hit = cache.get(key);
    if (hit) { setProfile(hit); return; }
    let cancelled = false;
    let p = pending.get(key);
    if (!p) {
      p = api.getMemberProfile(serverId, publicKey, profileHash!)
        .then((v): MemberProfile => {
          const result: MemberProfile = v
            ? { avatarUrl: v.avatar_data_url ?? null, status: v.status ?? null }
            : EMPTY;
          cache.set(key, result);
          pending.delete(key);
          return result;
        })
        .catch((): MemberProfile => { pending.delete(key); return EMPTY; });
      pending.set(key, p);
    }
    p.then(r => { if (!cancelled) setProfile(r); });
    return () => { cancelled = true; };
  }, [serverId, publicKey, profileHash]);

  return profile;
}
