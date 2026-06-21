import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { voiceRequestKeyframe } from "../lib/tauri-bridge";
import type { UseVoice } from "../hooks/useVoice";

interface StageFrame { pubkey?: string; data: string; key: boolean; seq?: number; }

// Constrained Baseline Level 5.1 — accepts 720p/1080p/1440p/4K30 streams.
// (The old "avc1.42E01E" was Level 3.0, which rejects anything above ~720p and
// silently blanked the canvas at higher share resolutions.)
const H264_CODEC = "avc1.42E033";
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64); const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// A single-canvas H.264 player: listens to `eventName`, decodes payloads that
// pass `match`, draws to the canvas. Key-gated; self-heals on decoder error.
// Only runs while `active`.
function useStreamPlayer(
  canvasRef: React.RefObject<HTMLCanvasElement>,
  eventName: string,
  match: (p: StageFrame) => boolean,
  active: boolean,
) {
  useEffect(() => {
    if (!active) return;
    let decoder: VideoDecoder | null = null;
    let gotKey = false;
    const ensure = () => {
      const canvas = canvasRef.current;
      if (!canvas || decoder) return;
      const ctx = canvas.getContext("2d")!;
      decoder = new VideoDecoder({
        output: (frame) => {
          canvas.width = frame.displayWidth; canvas.height = frame.displayHeight;
          ctx.drawImage(frame, 0, 0, canvas.width, canvas.height); frame.close();
        },
        error: (err) => { console.warn("[screenshare] decoder error, resetting:", err); try { decoder?.close(); } catch { /* ignore */ } decoder = null; gotKey = false; },
      });
      decoder.configure({ codec: H264_CODEC, optimizeForLatency: true });
    };
    const un = listen<StageFrame>(eventName, (e) => {
      const p = e.payload;
      if (!match(p)) return;
      ensure();
      if (!decoder) return;
      if (!gotKey && !p.key) return; // wait for a keyframe to start
      if (p.key) gotKey = true;
      try {
        decoder.decode(new EncodedVideoChunk({
          type: p.key ? "key" : "delta", timestamp: p.seq ?? 0, data: b64ToBytes(p.data),
        }));
      } catch { /* drop */ }
    });
    return () => { un.then((u) => u()); try { decoder?.close(); } catch { /* ignore */ } };
  }, [eventName, active]); // eslint-disable-line react-hooks/exhaustive-deps
}

export default function ScreenShareStage({ voice }: { voice: UseVoice }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ dx: number; dy: number } | null>(null);
  const [minimized, setMinimized] = useState(false);
  const [poppedOut, setPoppedOut] = useState(false);
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);

  // One sharer per channel: show your self-preview when sharing, else the peer you joined.
  const watchedPubkey = [...voice.watching][0] ?? null;
  const showSelf = voice.isSharing;
  const showPeer = !showSelf && watchedPubkey != null;
  const visible = showSelf || showPeer;

  // Decode in-app only while visible, not minimized, and not popped out
  // (when popped out, the detached window decodes instead). Minimize/pop-out
  // never stop the share — the encoder keeps running independently.
  useStreamPlayer(canvasRef, "voice://self-video-frame", () => true, showSelf && !minimized && !poppedOut);
  useStreamPlayer(canvasRef, "voice://peer-video-frame", (p) => p.pubkey === watchedPubkey, showPeer && !minimized && !poppedOut);

  // Reset transient panel state when the stream goes away entirely.
  useEffect(() => { if (!visible) setMinimized(false); }, [visible]);

  // The detached popout window tells us when it closes → restore the in-app panel.
  useEffect(() => {
    const un = listen("voice://popout-closed", () => setPoppedOut(false));
    return () => { un.then((u) => u()); };
  }, []);

  // When the popout (or a re-shown in-app panel) signals it's listening, force a
  // keyframe so it paints immediately instead of waiting for the periodic one.
  useEffect(() => {
    const un = listen("voice://popout-ready", () => { void voiceRequestKeyframe(); });
    return () => { un.then((u) => u()); };
  }, []);

  // Open the detached always-on-top preview window (one at a time).
  const openPopout = async () => {
    try {
      const existing = await WebviewWindow.getByLabel("screenshare-popout");
      if (existing) { await existing.setFocus(); setPoppedOut(true); return; }
      const w = new WebviewWindow("screenshare-popout", {
        url: "index.html?popout=screenshare",
        title: "Farder — Screen share",
        width: 640, height: 400, minWidth: 320, minHeight: 200,
        decorations: true, alwaysOnTop: true, resizable: true,
      });
      w.once("tauri://created", () => setPoppedOut(true));
      w.once("tauri://error", (e) => { console.error("[popout] create failed:", e); });
    } catch (e) {
      console.error("[popout] open failed:", e);
    }
  };
  const closePopout = async () => {
    try { const w = await WebviewWindow.getByLabel("screenshare-popout"); if (w) await w.close(); } catch { /* ignore */ }
    setPoppedOut(false);
  };

  // Close the popout if the stream ends while it's open.
  useEffect(() => { if (!visible && poppedOut) void closePopout(); }, [visible, poppedOut]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!visible) return null;

  // Drag by the header (but not when grabbing a button/slider).
  const startDrag = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest("button, input")) return;
    const el = (panelRef.current ?? (e.currentTarget as HTMLElement)).closest(".screen-stage, .screen-stage-mini") as HTMLElement | null;
    const rect = (el ?? (e.currentTarget as HTMLElement)).getBoundingClientRect();
    dragRef.current = { dx: e.clientX - rect.left, dy: e.clientY - rect.top };
    const onMove = (ev: MouseEvent) => {
      if (!dragRef.current) return;
      setPos({ x: ev.clientX - dragRef.current.dx, y: ev.clientY - dragRef.current.dy });
    };
    const onUp = () => {
      dragRef.current = null;
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  // When dragged, pin to explicit coords (overriding the CSS centering).
  const posStyle: React.CSSProperties | undefined = pos
    ? { left: pos.x, top: pos.y, right: "auto", transform: "none" }
    : undefined;
  const title = showSelf ? "You're sharing" : `${watchedPubkey?.slice(0, 8)}… is sharing`;

  // While popped out, the in-app panel collapses to a small restore pill.
  if (poppedOut) {
    return (
      <div className="screen-stage-mini" style={posStyle} onMouseDown={startDrag}>
        <span className="screen-stage-mini-dot" />
        <span className="screen-stage-mini-label">Popped out</span>
        <button className="screen-stage-min" title="Bring preview back into the app" onClick={() => void closePopout()}>&#x21F2;</button>
      </div>
    );
  }

  if (minimized) {
    return (
      <div className="screen-stage-mini" style={posStyle} onMouseDown={startDrag}>
        <span className="screen-stage-mini-dot" />
        <span className="screen-stage-mini-label">{showSelf ? "Sharing" : "Watching"}</span>
        <button className="screen-stage-min" title="Show preview" onClick={() => { setMinimized(false); void voiceRequestKeyframe(); }}>&#x25A2;</button>
      </div>
    );
  }

  return (
    <div className="screen-stage" ref={panelRef} style={posStyle}>
      <div className="screen-stage-head" onMouseDown={startDrag}>
        <span className="screen-stage-title">{title}</span>
        {showPeer && watchedPubkey && (
          <input
            className="screen-stage-vol" type="range" min={0} max={2} step={0.05} defaultValue={1}
            title="Game audio volume"
            onChange={(e) => voice.setGameAudioVolume(watchedPubkey, Number(e.target.value))}
          />
        )}
        <button className="screen-stage-min" title="Pop out into a floating window" onClick={() => void openPopout()}>&#x29C9;</button>
        <button className="screen-stage-min" title="Hide preview (keep sharing)" onClick={() => setMinimized(true)}>&#x2013;</button>
        <button
          className="screen-stage-close"
          title={showSelf ? "Stop sharing" : "Stop watching"}
          onClick={() => { if (showSelf) void voice.stopShare(); else if (watchedPubkey) voice.toggleWatch(watchedPubkey); }}
        >&#x2715;</button>
      </div>
      <canvas ref={canvasRef} className="screen-stage-canvas" />
    </div>
  );
}
