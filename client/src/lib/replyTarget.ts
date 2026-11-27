/** Choosing the `reply_to` event hash for a reply posted over the log.
 *  The reply bar holds a numeric message id; the log cites events by hash. */
export interface ReplyTargetResult {
  /** Event hash to pass as `reply_to`, or null for a top-level post. */
  replyTo: string | null;
  /** True when a reply was requested but the target has no hash (a row from
   *  before the log), so the message goes out top-level and the user is told. */
  tooOld: boolean;
}

export const REPLY_TOO_OLD_NOTE =
  "This message is too old to reply to here; sent as a normal message";

export function chooseReplyTarget(
  replyToId: number | undefined | null,
  messages: ReadonlyArray<{ id: number; event_hash?: string | null }>,
): ReplyTargetResult {
  if (replyToId === undefined || replyToId === null) return { replyTo: null, tooOld: false };
  const target = messages.find((m) => m.id === replyToId);
  if (target?.event_hash) return { replyTo: target.event_hash, tooOld: false };
  return { replyTo: null, tooOld: true };
}
