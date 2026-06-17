import { useEffect, useRef } from "react";
import { emit, listen } from "@tauri-apps/api/event";

// Standalone view rendered in the detached "screenshare-popout" OS window
// (main.tsx routes here when ?popout=screenshare is present). It does NOT mount
// the full app — it just decodes the same H.264 frame events the backend emits
// app-wide (reaching every window) and paints them to a canvas. Self-contained
// inline styling so it never depends on the theme/providers of the main window.

const H264_CODEC = "avc1.42E01E";
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
interface Frame { pubkey?: string; data: string; key: boolean; seq?: number; }

export default function ScreenSharePopout() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    let decoder: VideoDecoder | null = null;
    let gotKey = false;
    const ensure = () => {
      const canvas = canvasRef.current;
      if (!canvas || decoder) return;
      const ctx = canvas.getContext("2d")!;
      decoder = new VideoDecoder({
        output: (frame) => {
          canvas.width = frame.displayWidth;
          canvas.height = frame.displayHeight;
          ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
          frame.close();
        },
        error: () => { try { decoder?.close(); } catch { /* ignore */ } decoder = null; gotKey = false; },
      });
      decoder.configure({ codec: H264_CODEC, optimizeForLatency: true });
    };
    const handle = (p: Frame) => {
      ensure();
      if (!decoder) return;
      if (!gotKey && !p.key) return;
      if (p.key) gotKey = true;
      try {
        decoder.decode(new EncodedVideoChunk({ type: p.key ? "key" : "delta", timestamp: p.seq ?? 0, data: b64ToBytes(p.data) }));
      } catch { /* drop */ }
    };
    // One sharer per channel: only one of these streams flows at a time.
    const unSelf = listen<Frame>("voice://self-video-frame", (e) => handle(e.payload));
    const unPeer = listen<Frame>("voice://peer-video-frame", (e) => handle(e.payload));
    // Tell the main window we're listening so it forces a keyframe (no ~2s wait
    // for the periodic one), and let it restore the in-app panel when we close.
    void emit("voice://popout-ready");
    const onUnload = () => { void emit("voice://popout-closed"); };
    window.addEventListener("beforeunload", onUnload);
    return () => {
      unSelf.then((u) => u());
      unPeer.then((u) => u());
      try { decoder?.close(); } catch { /* ignore */ }
      window.removeEventListener("beforeunload", onUnload);
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      style={{ position: "fixed", inset: 0, width: "100%", height: "100%", background: "#000", objectFit: "contain", display: "block" }}
    />
  );
}
