import { useEffect, useRef } from "react";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { setFloatAnchor } from "../lib/floatAnchor";
import { useMediaPlayers, type MediaPlayerInfo } from "../context/MediaPlayersContext";

export default function MediaPlayer({ player }: { player: MediaPlayerInfo }) {
  const { hosts, focusPlayer, updatePlayer, setPlayerState, closePlayer } = useMediaPlayers();
  const rootRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ dx: number; dy: number; raf: number } | null>(null);

  // Video bytes via the relay (iframe needs no proxy). Don't fetch while minimized.
  const videoUrl = useProxiedMedia(
    player.kind === "video" ? player.src : null,
    player.kind === "video" && player.state !== "minimized",
  );

  // DOCKED: track the host slot's rect so the fixed element overlays it (looks inline).
  useEffect(() => {
    if (player.state !== "docked") return;
    let raf = 0;
    const place = () => {
      raf = 0;
      const host = player.hostId ? hosts.current.get(player.hostId) : null;
      const root = rootRef.current;
      if (!host || !root) return;
      const r = host.getBoundingClientRect();
      root.style.transform = `translate(${r.left}px, ${r.top}px)`;
      root.style.width = `${r.width}px`;
      root.style.height = `${r.height}px`;
    };
    const schedule = () => { if (!raf) raf = requestAnimationFrame(place); };
    place();
    window.addEventListener("scroll", schedule, true); // capture: catch the .message-list scroll
    window.addEventListener("resize", schedule);
    return () => {
      window.removeEventListener("scroll", schedule, true);
      window.removeEventListener("resize", schedule);
      if (raf) cancelAnimationFrame(raf);
      if (rootRef.current) rootRef.current.style.transform = "";
    };
  }, [player.state, player.hostId, hosts]);

  // FLOATING/ minimized drag via pointer capture (release outside the window still ends it).
  const startDrag = (e: React.PointerEvent) => {
    if ((e.target as HTMLElement).closest("button, input")) return;
    const root = rootRef.current;
    if (!root) return;
    const rect = root.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top, raf: 0 };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };
  const onDragMove = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    const x = e.clientX - d.dx, y = e.clientY - d.dy;
    if (!d.raf) d.raf = requestAnimationFrame(() => { d.raf = 0; updatePlayer(player.id, { pos: { x, y } }); });
  };
  const endDrag = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    if (d.raf) cancelAnimationFrame(d.raf);
    dragRef.current = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    setFloatAnchor({ x: player.pos.x, y: player.pos.y, w: player.size.w, h: player.size.h });
  };

  // Persist size after a CSS resize (floating only).
  useEffect(() => {
    if (player.state !== "floating") return;
    const el = rootRef.current;
    if (!el) return;
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

  const media = player.kind === "video"
    ? (videoUrl
        ? <video className="mp-media" src={videoUrl} controls autoPlay />
        : <div className="mp-state">Couldn&rsquo;t load video</div>)
    : <iframe
        className="mp-media"
        src={player.src}
        title={player.title}
        sandbox="allow-scripts allow-same-origin allow-presentation allow-popups"
        referrerPolicy="origin"
        allow="autoplay; encrypted-media; fullscreen; picture-in-picture"
        loading="lazy"
        allowFullScreen
      />;

  // MINIMIZED: pill
  if (player.state === "minimized") {
    return (
      <div className="mp-mini" style={{ left: player.pos.x, top: player.pos.y, zIndex: player.z }}
           onPointerDown={(e) => { focusPlayer(player.id); startDrag(e); }} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
        <span className="mp-title">{player.title}</span>
        <button className="mp-btn" title="Restore" onClick={() => setPlayerState(player.id, "floating")}>&#x25A2;</button>
        <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
      </div>
    );
  }

  // DOCKED: fixed element overlaying the slot; minimal chrome (pop-out only).
  if (player.state === "docked") {
    return (
      <div ref={rootRef} className="mp-docked" style={{ position: "fixed", left: 0, top: 0, zIndex: player.z }} onMouseDown={() => focusPlayer(player.id)}>
        {media}
        <button className="mp-pop" title="Pop out" onClick={() => setPlayerState(player.id, "floating")}>&#x2197;</button>
      </div>
    );
  }

  // FLOATING: full chrome.
  return (
    <div ref={rootRef} className="mp-float"
         style={{ left: player.pos.x, top: player.pos.y, width: player.size.w, height: player.size.h, opacity: player.opacity, zIndex: player.z }}
         onMouseDown={() => focusPlayer(player.id)}>
      <div className="mp-head" onPointerDown={startDrag} onPointerMove={onDragMove} onPointerUp={endDrag} onPointerCancel={endDrag}>
        <span className="mp-title">{player.title}</span>
        <input className="mp-opacity" type="range" min={0.2} max={1} step={0.05} value={player.opacity}
               title="Opacity" onChange={(e) => updatePlayer(player.id, { opacity: Number(e.target.value) })} />
        {player.hostId && <button className="mp-btn" title="Dock back into chat" onClick={() => setPlayerState(player.id, "docked")}>&#x21F2;</button>}
        <button className="mp-btn" title="Minimize" onClick={() => setPlayerState(player.id, "minimized")}>&#x2013;</button>
        <button className="mp-btn" title="Close" onClick={() => closePlayer(player.id)}>&#x2715;</button>
      </div>
      {media}
    </div>
  );
}
