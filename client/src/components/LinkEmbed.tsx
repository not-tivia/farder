import { useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useLinkEmbed } from "../hooks/useLinkEmbed";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { usePip } from "../context/PipContext";

export default function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean }) {
  // Data-saver: don't auto-load; show a chip that loads on click.
  const [loaded, setLoaded] = useState(!dataSaver);
  const state = useLinkEmbed(url, loaded);
  const { openPip } = usePip();

  // Derive embed properties safely (embed may be null until state is "ok").
  const e = state.status === "ok" ? state.embed : null;
  const inlineMedia = e?.media?.playable_inline ? e.media : null;
  const isVideo = !!inlineMedia?.mime.startsWith("video/");

  // IMPORTANT: both useProxiedMedia calls are hoisted here, before any early
  // return, so hook call order is stable across all render paths (rules of hooks).
  // The `enabled` flag gates the actual fetch — no work is done when not needed.
  //
  // Playable VIDEO no longer fetches its bytes on the card (it streams when the
  // PiP opens); only inline IMAGES fetch their media here.
  const mediaBlob = useProxiedMedia(
    inlineMedia?.url ?? null,
    loaded && state.status === "ok" && !!inlineMedia && !isVideo,
  );
  // Thumbnail: for non-inline providers (YouTube/Spotify) AND for the video poster.
  const thumbBlob = useProxiedMedia(
    e?.thumbnail ?? null,
    loaded && state.status === "ok" && !!e?.thumbnail && (!inlineMedia || isVideo),
  );

  // --- early returns (all hooks already called above) ---

  if (!loaded) {
    return (
      <button className="link-embed-chip" onClick={() => setLoaded(true)}>
        Load preview
      </button>
    );
  }
  if (state.status === "loading") {
    return <div className="link-embed link-embed-state">Loading preview&hellip;</div>;
  }
  if (state.status !== "ok" || !e) {
    // unsupported / unavailable: render nothing extra
    return null;
  }

  const openVideoPip = () => {
    if (!inlineMedia) return;
    openPip({ mediaUrl: inlineMedia.url, title: e.author ?? e.title ?? "Video", mime: inlineMedia.mime });
  };

  return (
    <div className={`link-embed link-embed--${e.provider}`}>
      {e.author && <div className="link-embed-author">{e.author}</div>}
      {e.title && <div className="link-embed-title">{e.title}</div>}
      {e.description && <div className="link-embed-desc">{e.description}</div>}

      {inlineMedia && isVideo && (
        <div className="link-embed-poster" onClick={openVideoPip}>
          {thumbBlob
            ? <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />
            : <div className="link-embed-poster-blank" />}
          <button
            className="link-embed-poster-play"
            onClick={(ev) => { ev.stopPropagation(); openVideoPip(); }}
          >
            &#9654; Play
          </button>
        </div>
      )}
      {inlineMedia && !isVideo && mediaBlob && (
        <img className="link-embed-image" src={mediaBlob} alt={e.title ?? ""} />
      )}
      {!inlineMedia && (thumbBlob || e.kind === "Video" || e.kind === "Audio") && (
        <div className="link-embed-thumb-wrap">
          {/* Thumbnail when it resolved; the Play/Open button renders regardless
              so a failed/absent thumbnail never hides the action (RC#2). */}
          {thumbBlob && <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />}
          {(e.kind === "Video" || e.kind === "Audio") && (
            <button
              className="link-embed-play"
              onClick={() => { void openExternal(e.url); }}
            >
              &#9654; {e.kind === "Video" ? "Play" : "Open"}
            </button>
          )}
        </div>
      )}
      {e.duration_secs != null && (
        <div className="link-embed-duration">{formatDuration(e.duration_secs)}</div>
      )}
    </div>
  );
}

function formatDuration(s: number): string {
  const m = Math.floor(s / 60);
  const sec = String(s % 60).padStart(2, "0");
  return `${m}:${sec}`;
}
