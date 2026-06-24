import { useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useLinkEmbed } from "../hooks/useLinkEmbed";
import { useProxiedMedia } from "../hooks/useProxiedMedia";
import { useMediaPlayers } from "../context/MediaPlayersContext";
import MediaSlot from "./MediaSlot";
import EmbedConsentModal from "./EmbedConsentModal";
import { buildEmbedPlayerSrc, getEmbedConsent, setEmbedConsent } from "../lib/embedPlayer";
import { getAlwaysFloat } from "../lib/floatAnchor";
import { useDataSaver } from "../context/DataSaverContext";

export default function LinkEmbed({ url }: { url: string }) {
  const { settings } = useDataSaver();
  // Captured once on mount (matches prior behavior); newly-rendered embeds
  // pick up a toggled setting, already-mounted ones keep their state.
  const [loaded, setLoaded] = useState(!settings.clickToLoadEmbeds);
  const state = useLinkEmbed(url, loaded);
  const { openPlayer } = useMediaPlayers();
  const [showConsent, setShowConsent] = useState(false);
  // Stable per-embed host id so the slot and player line up across re-renders.
  const [hostId] = useState(() => `embed-${Math.random().toString(36).slice(2)}`);

  const e = state.status === "ok" ? state.embed : null;
  const inlineMedia = e?.media?.playable_inline ? e.media : null;
  const isVideo = !!inlineMedia?.mime.startsWith("video/");

  // Inline IMAGE bytes (unchanged). Video no longer fetches on the card — it
  // streams when the player opens (inside MediaPlayer via useProxiedMedia).
  const imageBlob = useProxiedMedia(
    inlineMedia?.url ?? null,
    loaded && state.status === "ok" && !!inlineMedia && !isVideo,
  );
  // Thumbnail for the video / iframe poster.
  const thumbBlob = useProxiedMedia(
    e?.thumbnail ?? null,
    loaded && state.status === "ok" && !!e?.thumbnail && (!inlineMedia || isVideo),
  );

  if (!loaded) {
    return <button className="link-embed-chip" onClick={() => setLoaded(true)}>Load preview</button>;
  }
  if (state.status === "loading") {
    return <div className="link-embed link-embed-state">Loading preview&hellip;</div>;
  }
  if (state.status !== "ok" || !e) return null;

  const player = buildEmbedPlayerSrc(e.url);

  const watchHere = () => {
    if (!player) return;
    if (getEmbedConsent(player.provider)) openPlayer({ kind: "iframe", src: player.src, hostId, title: e.author ?? e.title ?? "Video", float: getAlwaysFloat() });
    else setShowConsent(true);
  };

  return (
    <div className={`link-embed link-embed--${e.provider}`}>
      {e.author && <div className="link-embed-author">{e.author}</div>}
      {e.title && <div className="link-embed-title">{e.title}</div>}
      {e.description && <div className="link-embed-desc">{e.description}</div>}

      {/* Inline image (unchanged) */}
      {inlineMedia && !isVideo && imageBlob && (
        <img className="link-embed-image" src={imageBlob} alt={e.title ?? ""} />
      )}

      {/* Playable VIDEO (Twitter/X video, direct file) -> inline-first player */}
      {inlineMedia && isVideo && (
        <MediaSlot hostId={hostId} kind="video" src={inlineMedia.url} title={e.author ?? e.title ?? "Video"} thumbUrl={thumbBlob} />
      )}

      {/* YouTube/Spotify -> "Watch here" opens an iframe player (after consent) */}
      {!inlineMedia && player && (
        <div className="link-embed-player-wrap">
          <MediaSlot hostId={hostId} kind="iframe" src={player.src} title={e.author ?? e.title ?? "Video"} thumbUrl={thumbBlob} aspect={player.provider === "spotify" ? 0.32 : 0.5625} manualTrigger />
          <div className="link-embed-slot-actions">
            <button className="embed-watch-btn" onClick={watchHere}>&#9654; Watch here</button>
            <button className="embed-open-external" onClick={() => { void openExternal(e.url); }}>Open externally &#8599;</button>
          </div>
          {player.provider === "spotify" && (
            <div className="embed-player-note">30-second preview in Farder &mdash; open externally for the full track.</div>
          )}
        </div>
      )}

      {/* Non-inline, non-YouTube/Spotify (e.g. reddit video) -> external open (unchanged) */}
      {!inlineMedia && !player && (thumbBlob || e.kind === "Video" || e.kind === "Audio") && (
        <div className="link-embed-thumb-wrap">
          {thumbBlob && <img className="link-embed-thumb" src={thumbBlob} alt={e.title ?? ""} />}
          {(e.kind === "Video" || e.kind === "Audio") && (
            <button className="link-embed-play" onClick={() => { void openExternal(e.url); }}>
              &#9654; {e.kind === "Video" ? "Play" : "Open"}
            </button>
          )}
        </div>
      )}

      {e.duration_secs != null && <div className="link-embed-duration">{formatDuration(e.duration_secs)}</div>}

      {showConsent && player && (
        <EmbedConsentModal
          provider={player.provider}
          onConfirm={(always) => {
            if (always) setEmbedConsent(player.provider, true);
            setShowConsent(false);
            openPlayer({ kind: "iframe", src: player.src, hostId, title: e.author ?? e.title ?? "Video", float: getAlwaysFloat() });
          }}
          onCancel={() => setShowConsent(false)}
        />
      )}
    </div>
  );
}

function formatDuration(s: number): string {
  const m = Math.floor(s / 60);
  const sec = String(s % 60).padStart(2, "0");
  return `${m}:${sec}`;
}
