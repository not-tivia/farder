import { useState } from "react";
import { useApp } from "../context/ServerContext";
import PollWidget from "./PollWidget";
import GiveawayWidget from "./GiveawayWidget";

/** Parsed `farder://widget/<kind>/<channel_id>/<widget_id>` link. The embedded
 *  channel id is client-side consistency data only — it is never sent to the
 *  server (the fetch is keyed by widget id alone, exactly like a widget card). */
export interface WidgetLink {
  kind: "poll" | "giveaway";
  channelId: number;
  widgetId: number;
}

interface LinkedWidgetCardProps {
  serverId: string;
  link: WidgetLink;
  /** Channel the containing message lives in — the viewer is by definition
   *  subscribed to it while reading, so same-channel cards get live events. */
  messageChannelId: number;
}

/**
 * Live, interactive widget card rendered under a message containing a widget
 * link. The mount fetch runs through the inner widget's `refetch` prop (always
 * `"mount"` or `"interval"` here, so it always fetches through the shipped
 * `getPoll`/`getGiveaway` state-recovery reads and dispatches `POLL_STATE`/
 * `GIVEAWAY_STATE` into the shared slice). Failure cases collapse into one
 * compact "not available" card:
 * - fetch error (nonexistent id OR a channel the viewer can't see — the server
 *   answers both with the same opaque "not found", so the card is identical in
 *   every failure case and the link can't act as an existence/content oracle);
 * - channel-id consistency mismatch (stale/forged/cross-server-pasted link
 *   whose widget id happens to exist on this server).
 */
export default function LinkedWidgetCard({ serverId, link, messageChannelId }: LinkedWidgetCardProps) {
  const { state } = useApp();
  const server = state.servers[serverId] ?? null;
  const info = link.kind === "poll"
    ? server?.polls[link.widgetId]?.poll ?? null
    : server?.giveaways[link.widgetId]?.giveaway ?? null;
  const [unavailable, setUnavailable] = useState(false);

  if (unavailable || (info && info.channel_id !== link.channelId)) {
    return (
      <div className="linked-widget-unavailable">
        {link.kind === "poll" ? "Poll not available" : "Giveaway not available"}
      </div>
    );
  }

  // Refetch discipline: PollUpdated/GiveawayUpdated broadcasts reach origin-
  // channel subscribers only, so a card outside that channel gets no live
  // events — poll every 20 s instead. Same-channel cards get the events and
  // only need the mount fetch (for the per-viewer my_vote/my_entered).
  const sameChannel = link.channelId === messageChannelId;
  const refetch = sameChannel ? "mount" : "interval";

  return link.kind === "poll" ? (
    <PollWidget
      serverId={serverId}
      pollId={link.widgetId}
      refetch={refetch}
      onUnavailable={() => setUnavailable(true)}
    />
  ) : (
    <GiveawayWidget
      serverId={serverId}
      giveawayId={link.widgetId}
      refetch={refetch}
      onUnavailable={() => setUnavailable(true)}
    />
  );
}
