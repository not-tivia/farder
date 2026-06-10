import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  getVoiceMode,
  setVoiceMode,
  getPttKey,
  setPttKey,
  getVoiceSensitivity,
  setVoiceSensitivity,
  startRecording,
  stopRecording,
  playAudioFile,
  type VoiceInputLevelPayload,
} from "../lib/tauri-bridge";
import SettingsSection from "./settings/SettingsSection";
import RadioOption from "./settings/RadioOption";
import KeybindRow from "./settings/KeybindRow";

// Keep in sync with sensitivity_to_threshold() in the Rust voice module.
function thresholdFor(sensitivity: number): number {
  return Math.max(0.0005, 0.05 - 0.049 * (Math.min(100, sensitivity) / 100));
}
// Map an RMS value to a 0-100% meter position (full scale = rms 0.1).
const meterPct = (rms: number) => Math.min(100, Math.max(0, rms * 1000));

type MicTestPhase = "idle" | "recording" | "playing";

const MIC_TEST_DURATION_MS = 3000;

export default function VoiceSettings() {
  const [mode, setMode] = useState<string>("OpenMic");
  const [pttKey, setPttKeyState] = useState<string>("Backquote");
  const [capturing, setCapturing] = useState(false);
  const [sensitivity, setSensitivity] = useState<number>(85);
  const [inputLevel, setInputLevel] = useState<number>(0);
  const [micTestPhase, setMicTestPhase] = useState<MicTestPhase>("idle");
  const [micTestError, setMicTestError] = useState<string | null>(null);

  useEffect(() => {
    void getVoiceMode().then(setMode).catch(() => {});
    void getPttKey().then(setPttKeyState).catch(() => {});
    void getVoiceSensitivity().then(setSensitivity).catch(() => {});
    // Live mic level (only flows while a voice call is active).
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<VoiceInputLevelPayload>("voice://input-level", (e) => setInputLevel(e.payload.level))
      .then((u) => { if (cancelled) u(); else unlisten = u; });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  const chooseMode = (next: string) => {
    setMode(next);
    void setVoiceMode(next).catch((e) => console.error("[voice-settings] failed to save mic mode:", e));
  };

  const chooseSensitivity = (v: number) => {
    setSensitivity(v);
    void setVoiceSensitivity(v).catch((e) => console.error("[voice-settings] failed to save sensitivity:", e));
  };

  useEffect(() => {
    if (!capturing) return;
    const onKey = (e: KeyboardEvent) => {
      // Capture phase + stopPropagation so the rebind swallows the keystroke
      // before the settings modal's window-level Escape handler can see it
      // (otherwise pressing Escape to bind a key would also close the modal).
      e.preventDefault();
      e.stopPropagation();
      setPttKeyState(e.code);
      void setPttKey(e.code).catch((err) => console.error("[voice-settings] failed to save PTT key:", err));
      setCapturing(false);
    };
    window.addEventListener("keydown", onKey, { once: true, capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [capturing]);

  const runMicTest = async () => {
    if (micTestPhase !== "idle") return;
    setMicTestError(null);
    setMicTestPhase("recording");
    try {
      await startRecording();
      await new Promise<void>((resolve) => setTimeout(resolve, MIC_TEST_DURATION_MS));
      const wavPath = await stopRecording();
      setMicTestPhase("playing");
      await playAudioFile(wavPath);
    } catch (err) {
      console.error("[voice-settings] mic test error:", err);
      setMicTestError(String(err));
    } finally {
      setMicTestPhase("idle");
    }
  };

  const micTestLabel =
    micTestPhase === "recording"
      ? "Recording..."
      : micTestPhase === "playing"
      ? "Playing back..."
      : "Test Mic";

  const threshold = thresholdFor(sensitivity);
  const levelPct = meterPct(inputLevel);
  const markerPct = meterPct(threshold);
  const isLoud = inputLevel > threshold;

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

      <div className="settings-divider" />
      <SettingsSection label="Mic Sensitivity">
        <div className="mic-meter">
          <div
            className={`mic-meter-fill${isLoud ? " active" : ""}`}
            style={{ width: `${levelPct}%` }}
          />
          <div className="mic-meter-marker" style={{ left: `${markerPct}%` }} />
        </div>
        <input
          type="range"
          min={0}
          max={100}
          step={1}
          value={sensitivity}
          onChange={(e) => chooseSensitivity(Number(e.target.value))}
          style={{ width: "100%" }}
        />
        <p className="settings-help">
          Drag so the bar turns green only when you talk, not when you're quiet.
          Join a voice channel to see your live mic level here.
        </p>
      </SettingsSection>

      <div className="settings-divider" />
      <SettingsSection label="Mic Test">
        <button
          className="btn btn-secondary"
          disabled={micTestPhase !== "idle"}
          onClick={() => void runMicTest()}
        >
          {micTestLabel}
        </button>
        <p className="settings-help">
          Records ~3 seconds from your mic and plays it back so you can hear how you sound.
        </p>
        {micTestError !== null && (
          <p className="settings-help" style={{ color: "var(--color-error, #f04747)" }}>
            Mic test failed: {micTestError}
          </p>
        )}
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
