import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
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
  const [title, setTitle] = useState("Screen share");

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
    const handle = (p: Frame, who: string) => {
      ensure();
      if (!decoder) return;
      if (!gotKey && !p.key) return;
      if (p.key) gotKey = true;
      setTitle(who);
      try {
        decoder.decode(new EncodedVideoChunk({ type: p.key ? "key" : "delta", timestamp: p.seq ?? 0, data: b64ToBytes(p.data) }));
      } catch { /* drop */ }
    };
    // One sharer per channel: only one of these streams flows at a time.
    const unSelf = listen<Frame>("voice://self-video-frame", (e) => handle(e.payload, "Your screen"));
    const unPeer = listen<Frame>("voice://peer-video-frame", (e) => handle(e.payload, `${e.payload.pubkey?.slice(0, 8) ?? "Peer"}… is sharing`));
    // Let the main window restore its in-app panel when this window closes.
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
    <div style={{ position: "fixed", inset: 0, display: "flex", flexDirection: "column", background: "#000", overflow: "hidden" }}>
      <div
        data-tauri-drag-region
        style={{ display: "flex", alignItems: "center", gap: 8, height: 26, padding: "0 8px", background: "#1c1d22", color: "#cfd3da", fontSize: 12, userSelect: "none", cursor: "move", flex: "0 0 auto" }}
      >
        <span style={{ flex: 1, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", pointerEvents: "none" }}>{title}</span>
        <button
          onClick={() => void getCurrentWindow().close()}
          title="Close preview window"
          style={{ background: "none", border: "none", color: "#cfd3da", cursor: "pointer", fontSize: 14, lineHeight: 1 }}
        >&#x2715;</button>
      </div>
      <canvas ref={canvasRef} style={{ flex: 1, width: "100%", minHeight: 0, background: "#000", objectFit: "contain", display: "block" }} />
    </div>
  );
}
