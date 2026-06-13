import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

// OpenH264 emits Annex-B (start codes); Chromium's VideoDecoder accepts it when
// configure() is called WITHOUT a `description`. Constrained Baseline 3.0.
const H264_CODEC = "avc1.42E01E";

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

interface FramePayload { session: string; pubkey: string; data: string; key: boolean; seq: number; }

// One decoder+canvas per sharing session. Lazily created on the first frame
// for a session; gated on the first keyframe.
class PeerDecoder {
  decoder: VideoDecoder;
  gotKey = false;
  constructor(canvas: HTMLCanvasElement, onError: (e: string) => void) {
    const ctx = canvas.getContext("2d")!;
    this.decoder = new VideoDecoder({
      output: (frame) => {
        canvas.width = frame.displayWidth;
        canvas.height = frame.displayHeight;
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
      },
      error: (e) => onError(String(e)),
    });
    this.decoder.configure({ codec: H264_CODEC, optimizeForLatency: true });
  }
  decode(p: FramePayload) {
    if (!this.gotKey && !p.key) return; // wait for a keyframe to start
    if (p.key) this.gotKey = true;
    try {
      this.decoder.decode(new EncodedVideoChunk({
        type: p.key ? "key" : "delta",
        timestamp: p.seq, // monotonic; advisory for a live stream
        data: b64ToBytes(p.data),
      }));
    } catch { /* drop */ }
  }
  close() { if (this.decoder.state !== "closed") this.decoder.close(); }
}

export default function PeerVideoTiles() {
  // session -> { pubkey }
  const [sessions, setSessions] = useState<Record<string, { pubkey: string }>>({});
  const canvasRefs = useRef<Record<string, HTMLCanvasElement | null>>({});
  const decoders = useRef<Record<string, PeerDecoder>>({});
  const lastSeen = useRef<Record<string, number>>({});

  useEffect(() => {
    const unlisten = listen<FramePayload>("voice://peer-video-frame", (e) => {
      const p = e.payload;
      lastSeen.current[p.session] = Date.now();
      setSessions((prev) => prev[p.session] ? prev : { ...prev, [p.session]: { pubkey: p.pubkey } });
      const dec = decoders.current[p.session];
      if (dec) {
        dec.decode(p);
      } else {
        const canvas = canvasRefs.current[p.session];
        if (canvas) {
          // Recreate a decoder that was dropped (first mount or post-error) and
          // feed this frame; it'll start once a keyframe arrives.
          const d = new PeerDecoder(canvas, () => { decoders.current[p.session]?.close(); delete decoders.current[p.session]; });
          decoders.current[p.session] = d;
          d.decode(p);
        }
        // else: canvas not mounted yet this tick; next frame will create it.
      }
    });
    return () => { unlisten.then((u) => u()); };
  }, []);

  // Reap sessions idle > 3s. Decoders are created synchronously in the canvas
  // ref callback (and re-created on demand in the frame listener), not here.
  useEffect(() => {
    const t = setInterval(() => {
      const now = Date.now();
      setSessions((prev) => {
        let changed = false;
        const next = { ...prev };
        for (const s of Object.keys(prev)) {
          if (now - (lastSeen.current[s] ?? 0) > 3000) {
            decoders.current[s]?.close();
            delete decoders.current[s];
            delete canvasRefs.current[s];
            delete lastSeen.current[s];
            delete next[s];
            changed = true;
          }
        }
        return changed ? next : prev;
      });
    }, 1000);
    return () => clearInterval(t);
  }, []);

  // Close ALL decoders on unmount so leaving the view doesn't leak VideoDecoders.
  useEffect(() => {
    return () => {
      for (const s of Object.keys(decoders.current)) { decoders.current[s]?.close(); }
      decoders.current = {};
    };
  }, []);

  const entries = Object.entries(sessions);
  if (entries.length === 0) return null;
  return (
    <div className="peer-video-tiles">
      {entries.map(([session, info]) => (
        <div key={session} className="peer-video-tile">
          <canvas
            ref={(el) => {
              canvasRefs.current[session] = el;
              if (el && !decoders.current[session]) {
                decoders.current[session] = new PeerDecoder(el, () => {
                  // Decoder errored (corrupt stream): drop it so the next frame
                  // for this session recreates it and re-gates on a keyframe.
                  decoders.current[session]?.close();
                  delete decoders.current[session];
                });
              }
            }}
            className="peer-video-canvas"
          />
          <div className="peer-video-label">{info.pubkey.slice(0, 8)}&hellip; is sharing</div>
        </div>
      ))}
    </div>
  );
}
