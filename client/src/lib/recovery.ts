// Recovery owner: the wording and timing the screens use. Pure, so it can be tested.
// See docs/recovery-owner-design.md.

export interface RecoveryPending {
  claim: string;
  new_owner: string;
  accepted_at: number;
  ready_at: number;
}

/** "3 days 4 hours", "5 hours 10 minutes", "12 minutes", "less than a minute". */
export function describeWait(totalSecs: number): string {
  const s = Math.max(0, Math.floor(totalSecs));
  if (s < 60) return "less than a minute";
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const unit = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;
  if (days > 0) return hours > 0 ? `${unit(days, "day")} ${unit(hours, "hour")}` : unit(days, "day");
  if (hours > 0) return minutes > 0 ? `${unit(hours, "hour")} ${unit(minutes, "minute")}` : unit(hours, "hour");
  return unit(minutes, "minute");
}

/** Where a waiting takeover stands, from the server's own times and the current time. */
export function takeoverState(p: RecoveryPending | null, nowSecs: number): { kind: "none" } | { kind: "waiting"; left: number } | { kind: "ready" } {
  if (!p) return { kind: "none" };
  if (nowSecs >= p.ready_at) return { kind: "ready" };
  return { kind: "waiting", left: p.ready_at - nowSecs };
}

/** The 24 words, cleaned up the way people paste them (extra spaces, line breaks, capitals). */
export function normalizePhrase(raw: string): string {
  return raw.toLowerCase().replace(/[^a-z\s]/g, " ").split(/\s+/).filter(Boolean).join(" ");
}

/** True when it looks like 24 words (the server and the app check the rest). */
export function looksLikeRecoveryPhrase(raw: string): boolean {
  return normalizePhrase(raw).split(" ").filter(Boolean).length === 24;
}
