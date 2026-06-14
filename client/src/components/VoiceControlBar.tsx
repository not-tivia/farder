import { useEffect, useState } from "react";
import type { UseVoice } from "../hooks/useVoice";
import { getVoiceMode, getPttKey } from "../lib/tauri-bridge";
import { classifyQuality } from "../lib/connectionQuality";

interface Props {
  voice: UseVoice;
  channelName: string;
  selfInitial: string;
  // Full leave (audio + presence). Falls back to audio-only voice.leave() if omitted.
  onDisconnect?: () => void;
}

export default function VoiceControlBar({ voice, channelName, selfInitial, onDisconnect }: Props) {
  const [micMode, setMicMode] = useState<string>("OpenMic");
  const [pttKey, setPttKey] = useState<string>("Backquote");

  useEffect(() => {
    void getVoiceMode().then(setMicMode).catch(() => {});
    void getPttKey().then(setPttKey).catch(() => {});
  }, []);

  if (!voice.inCall) return null;

  const q = voice.connectionQuality;
  const tier = q ? classifyQuality(q.rttMs, q.lossPct) : "unknown";
  const qualityTitle = q
    ? `Ping: ${Math.round(q.rttMs)} ms · Loss: ${q.lossPct.toFixed(1)}%`
    : "Measuring connection…";

  return (
    <div className="voice-control-bar">
      <div className="vcb-head">
        <span className="vcb-dot" /> Voice Connected
        <span className="vcb-channel"> &middot; {channelName}</span>
        <span
          className={`vcb-signal vcb-signal-${tier}`}
          title={qualityTitle}
          aria-label={qualityTitle}
        >
          <span className="vcb-signal-bar vcb-signal-bar-1" />
          <span className="vcb-signal-bar vcb-signal-bar-2" />
          <span className="vcb-signal-bar vcb-signal-bar-3" />
          <span className="vcb-signal-bar vcb-signal-bar-4" />
        </span>
      </div>
      <div className="vcb-self">
        <span className={`voice-avatar${voice.localSpeaking ? " speaking" : ""}`}>{selfInitial}</span>
        <span className="vcb-self-name">You</span>
        {voice.localSpeaking && <span className="vcb-self-status">speaking</span>}
      </div>
      {micMode === "PushToTalk" && (
        <button
          type="button"
          className={voice.transmitting ? "vcb-ptt vcb-ptt-on" : "vcb-ptt vcb-ptt-off"}
          onClick={() => { void voice.toggleTransmit(); }}
          title={voice.transmitting ? "Transmitting" : `Mic off · tap ${pttKey} to talk`}
        >
          <span className="vcb-ptt-dot" />
          {voice.transmitting ? "Transmitting" : `Tap ${pttKey} to talk`}
        </button>
      )}
      <div className="vcb-buttons">
        <button
          className={`vcb-btn${voice.muted ? " active" : ""}`}
          title={voice.muted ? "Unmute" : "Mute"}
          aria-pressed={voice.muted}
          onClick={() => voice.setMute(!voice.muted)}
        >{voice.muted ? <span>&#x1F507;</span> : <span>&#x1F399;</span>}</button>
        <button
          className={`vcb-btn${voice.deafened ? " active" : ""}`}
          title={voice.deafened ? "Undeafen" : "Deafen"}
          aria-pressed={voice.deafened}
          onClick={() => voice.setDeafen(!voice.deafened)}
        ><span>&#x1F3A7;</span></button>
        <select
          className="vcb-audio-source"
          title="Game-audio source (which output device to capture)"
          value={voice.audioDeviceId ?? ""}
          onChange={(e) => voice.setAudioDeviceId(e.target.value)}
          disabled={voice.isSharing}
        >
          {voice.audioDevices.map((d) => (
            <option key={d.id} value={d.id}>{d.name}{d.is_default ? " (default)" : ""}</option>
          ))}
        </select>
        <button
          className={`vcb-btn${voice.isSharing ? " active" : ""}`}
          title={voice.isSharing ? "Stop sharing your screen" : "Share your screen"}
          aria-pressed={voice.isSharing}
          onClick={() => { void (voice.isSharing ? voice.stopShare() : voice.startShare()); }}
        ><span>&#x1F5A5;</span></button>
        <button className="vcb-btn leave" title="Disconnect" onClick={() => (onDisconnect ? onDisconnect() : voice.leave())}><span>&#x2716;</span></button>
      </div>
      <div className="vcb-e2e"><span>&#x1F512;</span> End-to-end encrypted</div>
    </div>
  );
}
