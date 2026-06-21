import { useState } from "react";
import { useLinkEmbed } from "../hooks/useLinkEmbed";
import { useProxiedMedia } from "../hooks/useProxiedMedia";

export default function LinkEmbed({ url, dataSaver }: { url: string; dataSaver: boolean }) {
  // Data-saver: don't auto-load; show a chip that loads on click.
  const [loaded, setLoaded] = useState(!dataSaver);
  const state = useLinkEmbed(url, loaded);

  // Derive embed properties safely (embed may be null until state is "ok").
  const e = state.status === "ok" ? state.embed : null;
  const inlineMedia = e?.media?.playable_inline ? e.media : null;
  const isVideo = inlineMedia?.mime.startsWith("video/");

  // IMPORTANT: both useProxiedMedia calls are hoisted here, before any early
  // return, so hook call order is stable across all render paths (rules of hooks).
  // The `enabled` flag gates the actual fetch — no work is done when not needed.
  const mediaBlob = useProxiedMedia(
    inlineMedia?.url ?? null,
    loaded && state.status === "ok" && !!inlineMedia,
  );
  const thumbBlob = useProxiedMedia(
    e?.thumbnail ?? null,
    loaded && state.status === "ok" && !inlineMedia && !!e?.thumbnail,
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

  return (
    <div className={`link-embed link-embed--${e.provider}`}>
      {e.author && <div className="link-embed-author">{e.author}</div>}
      {e.title && <div className="link-embed-title">{e.title}</div>}
      {e.description && <div className="link-embed-desc">{e.description}</div>}

      {inlineMedia && isVideo && mediaBlob && (
        <video className="link-embed-video" src={mediaBlob} controls preload="metadata" />
      )}
      {inlineMedia && !isVideo && mediaBlob && (
        <img className="link-embed-image" src={mediaBlob} alt={e.title ?? ""} />
      )}
      {!inlineMedia && thumbBlob && (
        <div className="link-embed-thumb-wrap">
          <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />
          {(e.kind === "Video" || e.kind === "Audio") && (
            <button
              className="link-embed-play"
              onClick={() => window.open(e.url, "_blank")}
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
