// Replying from the notification pop-up.
//
// The pop-up is a separate, tiny window and must not carry its own copy of the
// "how do I send a message" rules (plaintext vs mesh vs DM encryption). It only
// ASKS: it emits `notify:reply` and the main window, which owns the app state,
// checks the request and sends it down the same paths the message box uses.
//
// This file is that checking and planning, kept pure (no DOM, no Tauri) so the
// parts that matter can be tested: a DM is encrypted before it leaves, an
// encrypted channel is refused, and a request naming something that does not
// exist is rejected rather than guessed at.

export const MAX_REPLY_CHARS = 2000;
export const REPLY_EVENT = "notify:reply";
export const REPLY_RESULT_EVENT = "notify:reply-result";

/** Where a pop-up reply goes. Set by the client only when replying is allowed. */
export interface ReplyTarget {
  server_id: string;
  channel_id: number;
}

export interface ReplyRequest {
  id: string;
  serverId: string;
  channelId: number;
  text: string;
}

export interface ReplyResult {
  id: string;
  ok: boolean;
  error?: string;
}

/** The slice of a server's state the planner needs. */
export interface ReplyServerView {
  channels: { id: number; class?: string }[];
  dms: { channel: { id: number }; peerKey: string }[];
  logServerId: string | null;
}

export type SendPlan =
  | { kind: "dm"; peerKey: string }
  | { kind: "mesh"; logServerId: string }
  | { kind: "legacy" };

export type Planned =
  | { ok: true; text: string; plan: SendPlan }
  | { ok: false; error: string };

/** Printable text only: no control characters (newlines are not allowed in a one-line reply either). */
export function cleanReplyText(raw: string): string {
  return Array.from(raw).filter((c) => !/[\p{Cc}\p{Cf}]/u.test(c)).join("").trim();
}

/** Check an untrusted `notify:reply` payload against what actually exists. */
export function planReply(req: unknown, view: ReplyServerView | undefined): Planned {
  const r = req as Partial<ReplyRequest> | null;
  if (!r || typeof r.id !== "string" || r.id.length === 0 || r.id.length > 64) return { ok: false, error: "Bad request." };
  if (typeof r.serverId !== "string" || typeof r.text !== "string") return { ok: false, error: "Bad request." };
  if (typeof r.channelId !== "number" || !Number.isInteger(r.channelId) || r.channelId < 0) return { ok: false, error: "Bad request." };

  const text = cleanReplyText(r.text);
  if (text.length === 0) return { ok: false, error: "Nothing to send." };
  if (Array.from(text).length > MAX_REPLY_CHARS) return { ok: false, error: `Too long (at most ${MAX_REPLY_CHARS} characters).` };

  if (!view) return { ok: false, error: "That server is not connected any more." };

  const dm = view.dms.find((d) => d.channel.id === r.channelId);
  if (dm) return { ok: true, text, plan: { kind: "dm", peerKey: dm.peerKey } };

  const channel = view.channels.find((c) => c.id === r.channelId);
  if (!channel) return { ok: false, error: "That channel does not exist any more." };
  if ((channel.class ?? "Plaintext") === "E2ee") {
    return { ok: false, error: "That channel is encrypted. Reply from the Farder window." };
  }
  return view.logServerId
    ? { ok: true, text, plan: { kind: "mesh", logServerId: view.logServerId } }
    : { ok: true, text, plan: { kind: "legacy" } };
}

/** The calls a send needs, injected so the real path can be tested. */
export interface SendDeps {
  sendMessage(serverId: string, channelId: number, content: string): Promise<unknown>;
  submitEvent(serverId: string, logServerId: string, channelId: number, content: string, replyTo: null, caps: never[]): Promise<unknown>;
  dmEncrypt(peerKey: string, plaintext: string): Promise<string>;
}

/** Send `text` the way the message box would for this plan. */
export async function executeReply(plan: SendPlan, req: ReplyRequest, text: string, deps: SendDeps): Promise<void> {
  switch (plan.kind) {
    case "dm": {
      // Encrypt FIRST; if that fails nothing is sent (never fall back to plaintext).
      const cipher = await deps.dmEncrypt(plan.peerKey, text);
      await deps.sendMessage(req.serverId, req.channelId, cipher);
      return;
    }
    case "mesh":
      await deps.submitEvent(req.serverId, plan.logServerId, req.channelId, text, null, []);
      return;
    case "legacy":
      await deps.sendMessage(req.serverId, req.channelId, text);
      return;
  }
}

/** Minimum gap between replies the main window will act on (a stuck key must not flood). */
export const MIN_REPLY_GAP_MS = 400;
