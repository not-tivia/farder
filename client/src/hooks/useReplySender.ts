import { useCallback, useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import {
  REPLY_EVENT, REPLY_RESULT_EVENT,
  type ReplyRequest, type ReplyResult, type ReplyTarget,
} from "../lib/quickReply";

export type ReplyState = "idle" | "sending" | "sent" | "error";

/**
 * Send a one-line reply from a small window (the pop-up or the edge strip).
 *
 * It does NOT send anything itself: it asks the main window (`notify:reply`),
 * which checks the request and sends it the way the message box does (encrypting
 * a DM first), then answers with `notify:reply-result`. `onSent` runs shortly
 * after a successful answer so "Sent" is visible for a moment.
 */
export function useReplySender(onSent: () => void) {
  const [state, setState] = useState<ReplyState>("idle");
  const [error, setError] = useState<string | null>(null);
  const pending = useRef<{ timer: number; unlisten: () => void } | null>(null);
  const onSentRef = useRef(onSent);
  onSentRef.current = onSent;

  useEffect(() => () => {
    if (pending.current) { pending.current.unlisten(); window.clearTimeout(pending.current.timer); }
  }, []);

  const send = useCallback(async (target: ReplyTarget, body: string) => {
    if (!body || state === "sending" || state === "sent") return;
    setState("sending");
    setError(null);
    const id = `r${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
    const finish = () => {
      if (pending.current) { pending.current.unlisten(); window.clearTimeout(pending.current.timer); pending.current = null; }
    };
    const unlisten = await listen<ReplyResult>(REPLY_RESULT_EVENT, (e) => {
      if (e.payload.id !== id) return;
      finish();
      if (e.payload.ok) {
        setState("sent");
        window.setTimeout(() => onSentRef.current(), 900);
      } else {
        setState("error");
        setError(e.payload.error ?? "Could not send.");
      }
    });
    const timer = window.setTimeout(() => {
      finish();
      setState("error");
      setError("Farder did not answer. Open the app and try again.");
    }, 10_000);
    pending.current = { timer, unlisten };
    const req: ReplyRequest = { id, serverId: target.server_id, channelId: target.channel_id, text: body };
    await emit(REPLY_EVENT, req);
  }, [state]);

  /** Back to idle, for a window that stays open after a send (the chat window). */
  const reset = useCallback(() => { setState("idle"); setError(null); }, []);

  return { state, error, send, reset };
}
