import { useEffect, useRef } from "react";
import { useMediaPlayers, type PlayerKind } from "../context/MediaPlayersContext";

export default function MediaSlot({
  hostId, kind, src, title, thumbUrl, aspect = 0.5625,
}: { hostId: string; kind: PlayerKind; src: string; title: string; thumbUrl?: string | null; aspect?: number }) {
  const { players, registerHost, unregisterHost, setHostVisible, openPlayer, setPlayerState } = useMediaPlayers();
  const ref = useRef<HTMLDivElement>(null);

  const player = players.find((p) => p.hostId === hostId);
  const docked = player?.state === "docked";

  // Register this slot's element + observe visibility for docked<->floating.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    registerHost(hostId, el);
    const io = new IntersectionObserver(
      (entries) => { for (const e of entries) setHostVisible(hostId, e.isIntersecting); },
      { threshold: 0.5 },
    );
    io.observe(el);
    return () => { io.disconnect(); unregisterHost(hostId); };
  }, [hostId]); // eslint-disable-line react-hooks/exhaustive-deps

  // The slot reserves the media's space (so the docked player overlays it and the
  // layout doesn't jump when it floats). Use a padding-top aspect box.
  return (
    <div ref={ref} className="media-slot" style={{ position: "relative", width: "100%", maxWidth: 480, marginTop: 6 }}>
      <div style={{ paddingTop: `${aspect * 100}%` }} />
      {!player && (
        <button className="media-slot-poster" onClick={() => openPlayer({ kind, src, hostId, title })}>
          {thumbUrl && <img className="media-slot-thumb" src={thumbUrl} alt={title} />}
          <span className="media-slot-play">&#9654; Play</span>
        </button>
      )}
      {player && !docked && (
        <button className="media-slot-chip" onClick={() => setPlayerState(player.id, "docked")}>
          &#9654; Playing in a floating player &mdash; dock it back
        </button>
      )}
      {/* When docked, the root-level MediaPlayer overlays this slot; nothing rendered here. */}
    </div>
  );
}
