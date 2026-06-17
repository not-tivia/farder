import { useEffect } from "react";
import type { UseVoice } from "../hooks/useVoice";

export default function ShareSetupPopover({ voice, onClose }: { voice: UseVoice; onClose: () => void }) {
  useEffect(() => { void voice.refreshDisplaySources(); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const screens = voice.displaySources.filter((s) => s.kind === "Screen");
  const windows = voice.displaySources.filter((s) => s.kind === "Window");

  return (
    <div className="share-modal-backdrop" onClick={onClose}>
    <div className="share-popover" role="dialog" aria-label="Start sharing" onClick={(e) => e.stopPropagation()}>
      <div className="share-popover-head">
        <span>Start sharing</span>
        <button className="share-popover-refresh" title="Refresh sources" onClick={() => void voice.refreshDisplaySources()}>&#x21BB;</button>
      </div>
      <div className="share-popover-sources">
        {screens.length > 0 && <div className="share-popover-group">&#x1F4FA; Screens</div>}
        {screens.map((s) => (
          <button key={s.id} className={`share-popover-item${voice.sourceId === s.id ? " selected" : ""}`} onClick={() => voice.setSourceId(s.id)}>{s.label}</button>
        ))}
        {windows.length > 0 && <div className="share-popover-group">&#x1FA9F; Windows</div>}
        {windows.map((s) => (
          <button key={s.id} className={`share-popover-item${voice.sourceId === s.id ? " selected" : ""}`} onClick={() => voice.setSourceId(s.id)}>{s.label}</button>
        ))}
        {voice.displaySources.length === 0 && <div className="share-popover-empty">No capture sources found.</div>}
      </div>
      <label className="share-popover-audio">
        <span>&#x1F50A; Game audio</span>
        <select value={voice.audioDeviceId ?? ""} onChange={(e) => voice.setAudioDeviceId(e.target.value)}>
          {voice.audioDevices.map((d) => (
            <option key={d.id} value={d.id}>{d.name}{d.is_default ? " (default)" : ""}</option>
          ))}
        </select>
      </label>
      <div className="share-popover-quality">
        <label>
          <span>Quality</span>
          <select value={voice.shareResId} onChange={(e) => voice.setShareResId(e.target.value as "720" | "1080" | "source")}>
            <option value="720">720p</option>
            <option value="1080">1080p</option>
            <option value="source">Source (no downscale)</option>
          </select>
        </label>
        <label>
          <span>FPS</span>
          <select value={voice.shareFps} onChange={(e) => voice.setShareFps(Number(e.target.value))}>
            <option value={30}>30</option>
            <option value={60}>60</option>
            <option value={120}>120</option>
          </select>
        </label>
      </div>
      <div className="share-popover-actions">
        <button className="share-popover-cancel" onClick={onClose}>Cancel</button>
        <button className="share-popover-go" disabled={!voice.sourceId} onClick={() => { void voice.startShare(); onClose(); }}>Go Live</button>
      </div>
    </div>
    </div>
  );
}
