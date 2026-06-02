import { useEffect, useState } from "react";
import { getVoiceMode, setVoiceMode, getPttKey, setPttKey } from "../lib/tauri-bridge";
import SettingsSection from "./settings/SettingsSection";
import RadioOption from "./settings/RadioOption";
import KeybindRow from "./settings/KeybindRow";

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
    <div className="settings-panel">
      <h2 className="settings-panel-title">Voice</h2>
      <SettingsSection label="Microphone Mode">
        <RadioOption
          selected={mode === "OpenMic"}
          label="Open Mic"
          description="Your mic is always live - transmit whenever you speak."
          onSelect={() => chooseMode("OpenMic")}
        />
        <RadioOption
          selected={mode === "PushToTalk"}
          label="Push to Talk"
          description="Stay muted until you press your key."
          onSelect={() => chooseMode("PushToTalk")}
        />
      </SettingsSection>
      {mode === "PushToTalk" && (
        <>
          <div className="settings-divider" />
          <SettingsSection label="Push-to-Talk Keybind">
            <KeybindRow
              label="Current key"
              keyLabel={pttKey}
              capturing={capturing}
              onRebind={() => setCapturing(true)}
            />
          </SettingsSection>
        </>
      )}
    </div>
  );
}
