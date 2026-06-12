import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

export interface MemberProfile {
  avatarUrl: string | null;
  status: string | null;
}

const EMPTY: MemberProfile = { avatarUrl: null, status: null };

// Profiles are immutable per hash, so a module-level cache is safe: each hash
// resolves over the wire at most once per app session.
const cache = new Map<string, MemberProfile>();
const pending = new Map<string, Promise<MemberProfile>>();

export function useMemberProfile(
  serverId: string,
  publicKey: string,
  profileHash?: string | null,
): MemberProfile {
  const [profile, setProfile] = useState<MemberProfile>(
    profileHash ? cache.get(profileHash) ?? EMPTY : EMPTY,
  );

  useEffect(() => {
    if (!profileHash) { setProfile(EMPTY); return; }
    const hit = cache.get(profileHash);
    if (hit) { setProfile(hit); return; }
    let cancelled = false;
    let p = pending.get(profileHash);
    if (!p) {
      p = api.getMemberProfile(serverId, publicKey, profileHash)
        .then((v): MemberProfile => {
          const result: MemberProfile = v
            ? { avatarUrl: v.avatar_data_url ?? null, status: v.status ?? null }
            : EMPTY;
          cache.set(profileHash, result);
          pending.delete(profileHash);
          return result;
        })
        .catch((): MemberProfile => { pending.delete(profileHash); return EMPTY; });
      pending.set(profileHash, p);
    }
    p.then(r => { if (!cancelled) setProfile(r); });
    return () => { cancelled = true; };
  }, [serverId, publicKey, profileHash]);

  return profile;
}
