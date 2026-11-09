import { useEffect, useRef } from "react";
import { emitTo, listen } from "@tauri-apps/api/event";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useApp } from "../context/ServerContext";
import { getOwnPk } from "./useServerEvents";
import {
  BUDDY_READY_EVENT, BUDDY_STATE_EVENT, CHAT_READ_EVENT, CHAT_READY_EVENT, CHAT_STATE_EVENT,
  buildBuddyState, buildChatState, parseChatRead, parseChatRef,
  type ChatRef,
} from "../lib/buddy";

/**
 * Feeds the buddy list and the chat windows (see lib/buddy.ts, buddy.rs).
 *
 * Mounted once in the main window. Those windows only DRAW; this hook is the one
 * that knows the conversations. It does nothing while no such window is open
 * (the check is one cheap look at the open windows, not building any text).
 * Requests coming back are from another window, so they are checked against
 * what really exists before anything happens.
 */
export function useSmallWindows(): void {
  const { state, dispatch } = useApp();
  const stateRef = useRef(state);
  stateRef.current = state;
  const chats = useRef(new Map<string, ChatRef>());
  const lastSent = useRef(new Map<string, string>());
  const timer = useRef<number | null>(null);

  async function exists(label: string): Promise<boolean> {
    // If the lookup itself fails we cannot tell, so assume the window is there
    // (sending to a closed one is harmless) rather than silently never sending.
    try { return (await WebviewWindow.getByLabel(label)) !== null; } catch (e) {
      console.warn("[buddy] could not look up window", label, e);
      return true;
    }
  }

  async function publish() {
    const st = stateRef.current;
    const own = await getOwnPk();
    // The buddy list.
    if (await exists("buddy")) {
      const bs = buildBuddyState(st.servers, st.serverList.map((s) => s.id), own);
      const json = JSON.stringify(bs);
      if (lastSent.current.get("buddy") !== json) {
        lastSent.current.set("buddy", json);
        console.log("[buddy] sending the list:", bs.entries.length, "entries");
        await emitTo("buddy", BUDDY_STATE_EVENT, bs).catch((e) => console.warn("[buddy] could not send the list:", e));
      }
    } else {
      lastSent.current.delete("buddy");
    }
    // Each open chat window gets its own conversation, addressed by label.
    for (const [label, ref] of [...chats.current]) {
      if (!(await exists(label))) { chats.current.delete(label); lastSent.current.delete(label); continue; }
      const cs = buildChatState(st.servers[ref.serverId], ref.serverId, ref.channelId, own);
      const json = JSON.stringify(cs);
      if (lastSent.current.get(label) === json) continue;
      lastSent.current.set(label, json);
      // A conversation that no longer exists is sent as `null` so the window says so.
      await emitTo(label, CHAT_STATE_EVENT, cs).catch((e) => console.warn("[chat] could not send the conversation:", e));
    }
  }

  function schedule() {
    if (timer.current !== null) return;
    timer.current = window.setTimeout(() => { timer.current = null; void publish(); }, 200);
  }

  // Anything in the app's state may change what these windows show.
  useEffect(() => { schedule(); }, [state]);

  useEffect(() => {
    const un: (() => void)[] = [];
    let cancelled = false;
    const keep = (p: Promise<() => void>) => p.then((u) => { if (cancelled) u(); else un.push(u); });
    keep(listen(BUDDY_READY_EVENT, () => { console.log("[buddy] list asked for its state"); lastSent.current.delete("buddy"); schedule(); }));
    keep(listen(CHAT_READY_EVENT, (e) => {
      const ref = parseChatRef(e.payload);
      if (!ref) return;
      chats.current.set(ref.label, ref);
      lastSent.current.delete(ref.label);
      schedule();
    }));
    keep(listen(CHAT_READ_EVENT, (e) => {
      const r = parseChatRead(e.payload);
      if (!r) return;
      const server = stateRef.current.servers[r.serverId];
      const known = server && (server.dms.some((d) => d.channel.id === r.channelId) || server.channels.some((c) => c.id === r.channelId));
      if (!known) return;
      if (r.lastId <= (server.readState[r.channelId] ?? 0)) return;
      dispatch({ type: "MARK_READ", serverId: r.serverId, payload: { channelId: r.channelId, lastMessageId: r.lastId } });
    }));
    return () => {
      cancelled = true;
      un.forEach((u) => u());
      // Clear AND forget the timer: React (dev) mounts, unmounts and remounts this
      // effect once, and a cancelled timer still marked pending made `schedule`
      // return early forever, so nothing was ever sent to the small windows.
      if (timer.current !== null) { window.clearTimeout(timer.current); timer.current = null; }
    };
  }, []);

}
