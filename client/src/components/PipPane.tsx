import { useEffect, useRef } from "react";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import type { PipPaneState, PipPatch } from "../context/PipContext";

interface Props {
  pane: PipPaneState;
  onClose: (id: string) => void;
  onFocus: (id: string) => void;
  onUpdate: (id: string, patch: PipPatch) => void;
}

export default function PipPane({ pane, onClose, onFocus, onUpdate }: Props) {
  const paneRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ dx: number; dy: number } | null>(null);

  // Stream the relay-proxied bytes → blob URL (same path the inline player used).
  // Don't fetch while minimized; useProxiedMedia revokes the blob URL on cleanup.
  const mediaUrl = useProxiedMedia(pane.mediaUrl, !pane.minimized);

  // Persist user CSS `resize: both` dimensions back into state.
  useEffect(() => {
    const el = paneRef.current;
    if (!el || pane.minimized) return;
    const ro = new ResizeObserver(() => {
      const w = el.offsetWidth, h = el.offsetHeight;
      if (w && h && (w !== pane.size.w || h !== pane.size.h)) onUpdate(pane.id, { size: { w, h } });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [pane.id, pane.minimized]); // eslint-disable-line react-hooks/exhaustive-deps

  // Drag by the header (ignore clicks on a button/slider).
  const startDrag = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest("button, input")) return;
    const el = paneRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top };
    const onMove = (ev: MouseEvent) => {
      if (!dragRef.current) return;
      onUpdate(pane.id, { pos: { x: ev.clientX - dragRef.current.dx, y: ev.clientY - dragRef.current.dy } });
    };
    const onUp = () => {
      dragRef.current = null;
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  if (pane.minimized) {
    return (
      <div
        className="pip-pane-mini"
        style={{ left: pane.pos.x, top: pane.pos.y, zIndex: pane.z }}
        onMouseDown={(e) => { onFocus(pane.id); startDrag(e); }}
      >
        <span className="pip-pane-title">{pane.title}</span>
        <button className="pip-pane-min" title="Restore" onClick={() => onUpdate(pane.id, { minimized: false })}>&#x25A2;</button>
        <button className="pip-pane-close" title="Close" onClick={() => onClose(pane.id)}>&#x2715;</button>
      </div>
    );
  }

  return (
    <div
      ref={paneRef}
      className="pip-pane"
      style={{ left: pane.pos.x, top: pane.pos.y, width: pane.size.w, height: pane.size.h, opacity: pane.opacity, zIndex: pane.z }}
      onMouseDown={() => onFocus(pane.id)}
    >
      <div className="pip-pane-head" onMouseDown={startDrag}>
        <span className="pip-pane-title">{pane.title}</span>
        <input
          className="pip-pane-opacity" type="range" min={0.2} max={1} step={0.05} value={pane.opacity}
          title="Opacity"
          onChange={(e) => onUpdate(pane.id, { opacity: Number(e.target.value) })}
        />
        <button className="pip-pane-min" title="Minimize" onClick={() => onUpdate(pane.id, { minimized: true })}>&#x2013;</button>
        <button className="pip-pane-close" title="Close" onClick={() => onClose(pane.id)}>&#x2715;</button>
      </div>
      {mediaUrl
        ? <video className="pip-pane-video" src={mediaUrl} controls autoPlay />
        : <div className="pip-pane-state">Couldn&rsquo;t load video</div>}
    </div>
  );
}
