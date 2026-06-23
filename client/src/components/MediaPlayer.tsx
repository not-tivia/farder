import { useEffect, useRef } from "react";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { setFloatAnchor } from "../lib/floatAnchor";
import { getMediaVolume, setMediaVolume } from "../lib/mediaPrefs";
import { useMediaPlayers, type MediaPlayerInfo } from "../context/MediaPlayersContext";

export default function MediaPlayer({ player }: { player: MediaPlayerInfo }) {
  const { focusPlayer, updatePlayer, setPlayerState, closePlayer } = useMediaPlayers();
  const rootRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const dragRef = useRef<{ dx: number; dy: number; raf: number } | null>(null);

  // Video bytes via the relay; kept loaded in ALL states (incl. minimized) so the
  // element never reloads. iframe needs no proxy.
  const videoUrl = useProxiedMedia(player.kind === "video" ? player.src : null, player.kind === "video");

  // Apply remembered volume once the <video> + src are ready; persist on change.
  useEffect(() => {
    if (player.kind === "video" && videoRef.current) videoRef.current.volume = getMediaVolume();
  }, [videoUrl, player.kind]);
  const onVolume = () => { if (videoRef.current) setMediaVolume(videoRef.current.volume); };

  // Drag (floating/minimized) via pointer capture — release outside the window still ends it.
  const startDrag = (e: React.PointerEvent) => {
    if ((e.target as HTMLElement).closest("button, input")) return;
    const root = rootRef.current; if (!root) return;
    const rect = root.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top, raf: 0 };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };
  const onDragMove = (e: React.PointerEvent) => {
    const d = dragRef.current; if (!d) return;
    const x = e.clientX - d.dx, y = e.clientY - d.dy;
    if (!d.raf) d.raf = requestAnimationFrame(() => { d.raf = 0; updatePlayer(player.id, { pos: { x, y } }); });
  };
  const endDrag = (e: React.PointerEvent) => {
    const d = dragRef.current; if (!d) return;
    if (d.raf) cancelAnimationFrame(d.raf);
    dragRef.current = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    setFloatAnchor({ x: player.pos.x, y: player.pos.y, w: player.size.w, h: player.size.h });
  };

  // Persist size after a CSS resize (floating only).
  useEffect(() => {
    if (player.state !== "floating") return;
    const el = rootRef.current; if (!el) return;
    const ro = new ResizeObserver(() => {
      const w = el.offsetWidth, h = el.offsetHeight;
      if (w && h && (w !== player.size.w || h !== player.size.h)) {
        updatePlayer(player.id, { size: { w, h } });
        setFloatAnchor({ x: player.pos.x, y: player.pos.y, w, h });
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [player.state, player.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // The media element — rendered ONCE; never repositioned in the DOM.
  const media = player.kind === "video"
    ? (videoUrl
        ? <video ref={videoRef} className="mp-media" src={videoUrl} controls autoPlay onVolumeChange={onVolume} />
        : <div className="mp-state">Couldn&rsquo;t load video</div>)
    : <iframe className="mp-media" src={player.src} title={player.title}
        sandbox="allow-scripts allow-same-origin allow-presentation allow-popups"
        referrerPolicy="origin" allow="autoplay; encrypted-media; fullscreen; picture-in-picture"
        loading="lazy" allowFullScreen />;

  const floating = player.state === "floating";
  const minimized = player.state === "minimized";
  const cls = minimized ? "mp-mini" : floating ? "mp-float" : "mp-docked";
  const style: React.CSSProperties | undefined = floating
    ? { left: player.pos.x, top: player.pos.y, width: player.size.w, height: player.size.h, opacity: player.opacity, zIndex: player.z }
    : minimized
      ? { left: player.pos.x, top: player.pos.y, zIndex: player.z }
      : { zIndex: player.z }; // docked: position/inset come from .mp-docked CSS

  return (
    <div ref={rootRef} className={cls} style={style} onMouseDown={(floating || minimized) ? () => focusPlayer(player.id) : undefined}>
      {floating && (
        <div className="mp-head" onPointerDown={startDrag} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
          <span className="mp-title">{player.title}</span>
          <input className="mp-opacity" type="range" min={0.2} max={1} step={0.05} value={player.opacity}
                 title="Opacity" onChange={(e) => updatePlayer(player.id, { opacity: Number(e.target.value) })} />
          {player.hostId && <button className="mp-btn" title="Dock back into chat" onClick={() => setPlayerState(player.id, "docked")}>&#x21F2;</button>}
          <button className="mp-btn" title="Minimize" onClick={() => setPlayerState(player.id, "minimized")}>&#x2013;</button>
          <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
        </div>
      )}
      {minimized && (
        <div className="mp-mini-bar" onPointerDown={startDrag} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
          <span className="mp-title">{player.title}</span>
          <button className="mp-btn" title="Restore" onClick={() => setPlayerState(player.id, "floating")}>&#x25A2;</button>
          <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
        </div>
      )}
      {!floating && !minimized && (
        <button className="mp-pop" title="Pop out" onClick={() => setPlayerState(player.id, "floating")}>&#x2197;</button>
      )}
      <div className="mp-media-wrap" style={minimized ? { display: "none" } : undefined}>
        {media}
      </div>
    </div>
  );
}
