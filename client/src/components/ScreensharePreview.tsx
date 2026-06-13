import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../lib/tauri-bridge";

// OpenH264 emits Annex-B (start codes); Chromium's VideoDecoder accepts it when
// configure() is called WITHOUT a `description`. Constrained Baseline 3.0.
const H264_CODEC = "avc1.42E01E";

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export default function ScreensharePreview() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const gotKeyRef = useRef(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!("VideoDecoder" in window)) {
      setError("WebCodecs VideoDecoder is not available in this webview.");
    }
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<{ data: string; key: boolean; ts: number }>(
      "screenshare:frame",
      (e) => {
        const dec = decoderRef.current;
        if (!dec) return;
        const { data, key, ts } = e.payload;
        // The decoder must start on a keyframe; ignore deltas until the first key.
        if (!gotKeyRef.current && !key) return;
        if (key) gotKeyRef.current = true;
        try {
          const chunk = new EncodedVideoChunk({
            type: key ? "key" : "delta",
            timestamp: ts * 1000, // ms -> us
            data: b64ToBytes(data),
          });
          dec.decode(chunk);
        } catch (err) {
          setError(String(err));
        }
      },
    );
    return () => { unlistenPromise.then((u) => u()); };
  }, []);

  async function start() {
    setError(null);
    gotKeyRef.current = false;
    const canvas = canvasRef.current!;
    const ctx = canvas.getContext("2d")!;
    const decoder = new VideoDecoder({
      output: (frame) => {
        canvas.width = frame.displayWidth;
        canvas.height = frame.displayHeight;
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
      },
      error: (err) => setError(String(err)),
    });
    decoder.configure({ codec: H264_CODEC, optimizeForLatency: true });
    decoderRef.current = decoder;
    try {
      await api.startScreensharePreview(30, 1280, 720);
      setRunning(true);
    } catch (err) {
      setError(String(err));
    }
  }

  async function stop() {
    try { await api.stopScreensharePreview(); } catch {}
    setRunning(false);
    const dec = decoderRef.current;
    if (dec && dec.state !== "closed") dec.close();
    decoderRef.current = null;
    gotKeyRef.current = false;
  }

  return (
    <div className="screenshare-preview">
      <div className="screenshare-preview-controls">
        {running ? (
          <button className="xp-button" onClick={stop}>Stop preview</button>
        ) : (
          <button className="xp-button" onClick={start}>Start screen preview</button>
        )}
      </div>
      {error && <div className="screenshare-preview-error">{error}</div>}
      <canvas ref={canvasRef} className="screenshare-preview-canvas" />
    </div>
  );
}
