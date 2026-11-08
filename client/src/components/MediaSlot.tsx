import { useEffect, useRef } from "react";
import { useMediaPlayers, type PlayerKind } from "../context/MediaPlayersContext";
import { getAlwaysFloat } from "../lib/floatAnchor";
import MediaPlayer from "./MediaPlayer";

export default function MediaSlot({
  hostId, kind, src, title, thumbUrl, aspect = 0.5625, manualTrigger = false, autoplay = false,
}: { hostId: string; kind: PlayerKind; src: string; title: string; thumbUrl?: string | null; aspect?: number; manualTrigger?: boolean; autoplay?: boolean }) {
  const { players, setHostVisible, openPlayer, setPlayerState, closePlayer } = useMediaPlayers();
  const ref = useRef<HTMLDivElement>(null);
  const player = players.find((p) => p.hostId === hostId);

  // Keep the current player id in a ref so the unmount cleanup can close it.
  const playerIdRef = useRef<string | null>(null);
  playerIdRef.current = player?.id ?? null;

  // VIDEO auto-floats when its card scrolls out of view (and docks back). IFRAME
  // does not auto-float (manual pop-out only), so only observe for video.
  useEffect(() => {
    if (kind !== "video") return;
    const el = ref.current; if (!el) return;
    const io = new IntersectionObserver(
      (entries) => { for (const e of entries) setHostVisible(hostId, e.isIntersecting); },
      { threshold: 0.5 },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hostId, kind]); // eslint-disable-line react-hooks/exhaustive-deps

  // AUTOPLAY (trusted sender, already decided by the caller): start the video once,
  // muted, when at least half of it is on screen. Video only -- an iframe needs
  // consent and never starts by itself. It starts at most once per card, so
  // closing it is final until the message is re-rendered.
  const autoStarted = useRef(false);
  useEffect(() => {
    if (!autoplay || kind !== "video") return;
    const el = ref.current; if (!el) return;
    const io = new IntersectionObserver((entries) => {
      if (autoStarted.current) return;
      if (entries.some((e) => e.isIntersecting)) {
        autoStarted.current = true;
        openPlayer({ kind, src, hostId, title, float: false, quiet: true });
        io.disconnect();
      }
    }, { threshold: 0.5 });
    io.observe(el);
    return () => io.disconnect();
  }, [autoplay, kind, src, hostId, title]); // eslint-disable-line react-hooks/exhaustive-deps

  // On unmount (e.g. switching server/channel) close this slot's player — floating
  // players live with their message and do not persist across navigation.
  useEffect(() => {
    return () => { if (playerIdRef.current) closePlayer(playerIdRef.current); };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div ref={ref} className="media-slot" style={{ position: "relative", width: "100%", maxWidth: 480, marginTop: 6 }}>
      <div style={{ paddingTop: `${aspect * 100}%` }} />
      {!player && !manualTrigger && (
        <button className="media-slot-poster" onClick={() => openPlayer({ kind, src, hostId, title, float: getAlwaysFloat() })}>
          {thumbUrl && <img className="media-slot-thumb" src={thumbUrl} alt={title} />}
          <span className="media-slot-play">&#9654; Play</span>
        </button>
      )}
      {player && <MediaPlayer player={player} />}
      {player && player.state !== "docked" && (
        <button className="media-slot-chip" onClick={() => setPlayerState(player.id, "docked")}>
          &#9654; Playing in a floating player &mdash; dock it back
        </button>
      )}
    </div>
  );
}
