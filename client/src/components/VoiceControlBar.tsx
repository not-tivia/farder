import type { UseVoice } from "../hooks/useVoice";

interface Props {
  voice: UseVoice;
  channelName: string;
  selfInitial: string;
  // Full leave (audio + presence). Falls back to audio-only voice.leave() if omitted.
  onDisconnect?: () => void;
}

export default function VoiceControlBar({ voice, channelName, selfInitial, onDisconnect }: Props) {
  if (!voice.inCall) return null;
  return (
    <div className="voice-control-bar">
      <div className="vcb-head">
        <span className="vcb-dot" /> Voice Connected
        <span className="vcb-channel"> &middot; {channelName}</span>
      </div>
      <div className="vcb-self">
        <span className={`voice-avatar${voice.localSpeaking ? " speaking" : ""}`}>{selfInitial}</span>
        <span className="vcb-self-name">You</span>
        {voice.localSpeaking && <span className="vcb-self-status">speaking</span>}
      </div>
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
        <button className="vcb-btn leave" title="Disconnect" onClick={() => (onDisconnect ? onDisconnect() : voice.leave())}><span>&#x2716;</span></button>
      </div>
      <div className="vcb-e2e"><span>&#x1F512;</span> End-to-end encrypted</div>
    </div>
  );
}
