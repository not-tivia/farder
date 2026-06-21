import { useEffect, useState } from "react";
import { getLinkEmbed } from "../lib/tauri-bridge";
import type { LinkEmbed } from "../lib/linkEmbed";

type State =
  | { status: "loading"; embed: null }
  | { status: "ok"; embed: LinkEmbed }
  | { status: "unsupported" | "unavailable"; embed: null };

export function useLinkEmbed(url: string, enabled: boolean): State {
  const [state, setState] = useState<State>({ status: "loading", embed: null });
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    setState({ status: "loading", embed: null });
    getLinkEmbed(url)
      .then((out) => {
        if (!alive) return;
        if (typeof out === "object" && "Embed" in out) setState({ status: "ok", embed: out.Embed });
        else if (out === "Unsupported") setState({ status: "unsupported", embed: null });
        else setState({ status: "unavailable", embed: null });
      })
      .catch(() => { if (alive) setState({ status: "unavailable", embed: null }); });
    return () => { alive = false; };
  }, [url, enabled]);
  return state;
}
