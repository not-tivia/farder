import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { UseVoice } from "../hooks/useVoice";

interface StageFrame { pubkey?: string; data: string; key: boolean; seq?: number; }

const H264_CODEC = "avc1.42E01E";
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
        error: () => { try { decoder?.close(); } catch { /* ignore */ } decoder = null; gotKey = false; },
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
  // One sharer per channel: show your self-preview when sharing, else the peer you joined.
  const watchedPubkey = [...voice.watching][0] ?? null;
  const showSelf = voice.isSharing;
  const showPeer = !showSelf && watchedPubkey != null;

  useStreamPlayer(canvasRef, "voice://self-video-frame", () => true, showSelf);
  useStreamPlayer(canvasRef, "voice://peer-video-frame", (p) => p.pubkey === watchedPubkey, showPeer);

  if (!showSelf && !showPeer) return null;
  return (
    <div className="screen-stage">
      <div className="screen-stage-head">
        <span className="screen-stage-title">{showSelf ? "You're sharing" : `${watchedPubkey?.slice(0, 8)}… is sharing`}</span>
        {showPeer && watchedPubkey && (
          <input
            className="screen-stage-vol" type="range" min={0} max={2} step={0.05} defaultValue={1}
            title="Game audio volume"
            onChange={(e) => voice.setGameAudioVolume(watchedPubkey, Number(e.target.value))}
          />
        )}
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
