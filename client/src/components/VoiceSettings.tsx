import { useEffect, useState } from "react";
import { getVoiceMode, setVoiceMode, getPttKey, setPttKey } from "../lib/tauri-bridge";

export default function VoiceSettings() {
  const [mode, setMode] = useState<string>("OpenMic");
  const [pttKey, setPttKeyState] = useState<string>("Backquote");
  const [capturing, setCapturing] = useState(false);

  useEffect(() => {
    void getVoiceMode().then(setMode).catch(() => {});
    void getPttKey().then(setPttKeyState).catch(() => {});
  }, []);

  const chooseMode = (next: string) => {
    setMode(next);
    void setVoiceMode(next).catch(() => {});
  };

  useEffect(() => {
    if (!capturing) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      setPttKeyState(e.code);
      void setPttKey(e.code).catch(() => {});
      setCapturing(false);
    };
    window.addEventListener("keydown", onKey, { once: true });
    return () => window.removeEventListener("keydown", onKey);
  }, [capturing]);

  return (
    <div className="voice-settings">
      <h3>Microphone mode</h3>
      <label>
        <input
          type="radio"
          name="voice-mode"
          checked={mode === "OpenMic"}
          onChange={() => chooseMode("OpenMic")}
        />
        Open Mic
      </label>
      <label>
        <input
          type="radio"
          name="voice-mode"
          checked={mode === "PushToTalk"}
          onChange={() => chooseMode("PushToTalk")}
        />
        Push-to-Talk
      </label>
      {mode === "PushToTalk" && (
        <div className="voice-settings-ptt-key">
          <span>Key: {pttKey}</span>
          <button type="button" onClick={() => setCapturing(true)}>
            {capturing ? "Press a key..." : "Rebind"}
          </button>
        </div>
      )}
    </div>
  );
}
