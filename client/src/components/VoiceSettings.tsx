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
  listInputDevices,
  listOutputDevices,
  getInputDevice,
  setInputDevice,
  getOutputDevice,
  setOutputDevice,
  getDataSaverEmbeds,
  setDataSaverEmbeds,
  getPresenceEnabled,
  setPresenceEnabled,
  getPresenceMusic,
  setPresenceMusic,
  type AudioDeviceInfo,
  type VoiceInputLevelPayload,
} from "../lib/tauri-bridge";
import { getEmbedConsent, setEmbedConsent } from "../lib/embedPlayer";
import { getAlwaysFloat, setAlwaysFloat } from "../lib/floatAnchor";
import SettingsSection from "./settings/SettingsSection";
import RadioOption from "./settings/RadioOption";
import KeybindRow from "./settings/KeybindRow";
import ScreensharePreview from "./ScreensharePreview";

// Keep in sync with sensitivity_to_threshold() in the Rust voice module.
function thresholdFor(sensitivity: number): number {
  return Math.max(0.0005, 0.05 - 0.049 * (Math.min(100, sensitivity) / 100));
}
// Map an RMS value to a 0-100% meter position (full scale = rms 0.1).
const meterPct = (rms: number) => Math.min(100, Math.max(0, rms * 1000));

type MicTestPhase = "idle" | "recording" | "playing";

const MIC_TEST_DURATION_MS = 3000;

// System-default sentinel shown in the dropdown.
const SYSTEM_DEFAULT = "";

export default function VoiceSettings() {
  const [mode, setMode] = useState<string>("OpenMic");
  const [pttKey, setPttKeyState] = useState<string>("Backquote");
  const [capturing, setCapturing] = useState(false);
  const [sensitivity, setSensitivity] = useState<number>(85);
  const [inputLevel, setInputLevel] = useState<number>(0);
  const [micTestPhase, setMicTestPhase] = useState<MicTestPhase>("idle");
  const [micTestError, setMicTestError] = useState<string | null>(null);
  const [inputDevices, setInputDevices] = useState<AudioDeviceInfo[]>([]);
  const [outputDevices, setOutputDevices] = useState<AudioDeviceInfo[]>([]);
  // Empty string = system default.
  const [selectedInput, setSelectedInput] = useState<string>(SYSTEM_DEFAULT);
  const [selectedOutput, setSelectedOutput] = useState<string>(SYSTEM_DEFAULT);
  const [dataSaverEmbeds, setDataSaverEmbedsState] = useState<boolean>(false);
  const [ytEmbeds, setYtEmbeds] = useState<boolean>(false);
  const [spotifyEmbeds, setSpotifyEmbeds] = useState<boolean>(false);
  const [alwaysFloat, setAlwaysFloatState] = useState<boolean>(false);
  const [presenceEnabled, setPresenceEnabledState] = useState<boolean>(false);
  const [presenceMusic, setPresenceMusicState] = useState<boolean>(false);

  useEffect(() => {
    void getVoiceMode().then(setMode).catch(() => {});
    void getPttKey().then(setPttKeyState).catch(() => {});
    void getVoiceSensitivity().then(setSensitivity).catch(() => {});
    void listInputDevices().then(setInputDevices).catch(() => {});
    void listOutputDevices().then(setOutputDevices).catch(() => {});
    void getInputDevice().then((n) => setSelectedInput(n ?? SYSTEM_DEFAULT)).catch(() => {});
    void getOutputDevice().then((n) => setSelectedOutput(n ?? SYSTEM_DEFAULT)).catch(() => {});
    void getDataSaverEmbeds().then(setDataSaverEmbedsState).catch(() => {});
    void getPresenceEnabled().then(setPresenceEnabledState).catch(() => {});
    void getPresenceMusic().then(setPresenceMusicState).catch(() => {});
    // Live mic level (only flows while a voice call is active).
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<VoiceInputLevelPayload>("voice://input-level", (e) => setInputLevel(e.payload.level))
      .then((u) => { if (cancelled) u(); else unlisten = u; });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    setYtEmbeds(getEmbedConsent("youtube"));
    setSpotifyEmbeds(getEmbedConsent("spotify"));
  }, []);

  useEffect(() => { setAlwaysFloatState(getAlwaysFloat()); }, []);

  const chooseInputDevice = (name: string) => {
    setSelectedInput(name);
    const value = name === SYSTEM_DEFAULT ? null : name;
    void setInputDevice(value).catch((e) =>
      console.error("[voice-settings] failed to save input device:", e)
    );
  };

  const chooseOutputDevice = (name: string) => {
    setSelectedOutput(name);
    const value = name === SYSTEM_DEFAULT ? null : name;
    void setOutputDevice(value).catch((e) =>
      console.error("[voice-settings] failed to save output device:", e)
    );
  };

  const chooseMode = (next: string) => {
    setMode(next);
    void setVoiceMode(next).catch((e) => console.error("[voice-settings] failed to save mic mode:", e));
  };

  const chooseSensitivity = (v: number) => {
    setSensitivity(v);
    void setVoiceSensitivity(v).catch((e) => console.error("[voice-settings] failed to save sensitivity:", e));
  };

  const chooseDataSaverEmbeds = (enabled: boolean) => {
    setDataSaverEmbedsState(enabled);
    void setDataSaverEmbeds(enabled).catch((e) => console.error("[voice-settings] failed to save data-saver setting:", e));
  };
  const chooseYtEmbeds = (enabled: boolean) => {
    setYtEmbeds(enabled);
    setEmbedConsent("youtube", enabled);
  };
  const chooseSpotifyEmbeds = (enabled: boolean) => {
    setSpotifyEmbeds(enabled);
    setEmbedConsent("spotify", enabled);
  };
  const chooseAlwaysFloat = (v: boolean) => { setAlwaysFloatState(v); setAlwaysFloat(v); };
  const choosePresenceEnabled = (enabled: boolean) => {
    setPresenceEnabledState(enabled);
    void setPresenceEnabled(enabled).catch((e) => console.error("[voice-settings] failed to save presence-enabled:", e));
    // If master is turned off, also clear music
    if (!enabled) {
      setPresenceMusicState(false);
      void setPresenceMusic(false).catch(() => {});
    }
  };
  const choosePresenceMusic = (enabled: boolean) => {
    setPresenceMusicState(enabled);
    void setPresenceMusic(enabled).catch((e) => console.error("[voice-settings] failed to save presence-music:", e));
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
      const session = await startRecording();
      await new Promise<void>((resolve) => setTimeout(resolve, MIC_TEST_DURATION_MS));
      const wavPath = await stopRecording(session);
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

      <SettingsSection label="Microphone (input device)">
        <select
          value={selectedInput}
          onChange={(e) => chooseInputDevice(e.target.value)}
          style={{ width: "100%" }}
        >
          <option value={SYSTEM_DEFAULT}>System default</option>
          {inputDevices.map((d) => (
            <option key={d.name} value={d.name}>
              {d.name}{d.is_default ? " (default)" : ""}
            </option>
          ))}
        </select>
        <p className="settings-help">
          The microphone used for voice calls and voice message recording.
        </p>
      </SettingsSection>

      <div className="settings-divider" />
      <SettingsSection label="Output device (speakers / headphones)">
        <select
          value={selectedOutput}
          onChange={(e) => chooseOutputDevice(e.target.value)}
          style={{ width: "100%" }}
        >
          <option value={SYSTEM_DEFAULT}>System default</option>
          {outputDevices.map((d) => (
            <option key={d.name} value={d.name}>
              {d.name}{d.is_default ? " (default)" : ""}
            </option>
          ))}
        </select>
        <p className="settings-help">
          The device used to play incoming voice and mic test playback.
        </p>
      </SettingsSection>

      <div className="settings-divider" />
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

      <div className="settings-divider" />
      <SettingsSection label="Screen Share (preview &mdash; Phase B)">
        <ScreensharePreview />
      </SettingsSection>

      <div className="settings-divider" />
      <SettingsSection label="Privacy &amp; Data">
        <label className="settings-row">
          <input
            type="checkbox"
            checked={dataSaverEmbeds}
            onChange={(e) => chooseDataSaverEmbeds(e.target.checked)}
          />
          Data saver: load link previews only when clicked
        </label>
        <p className="settings-help">
          When enabled, link preview cards show a "Load preview" button instead of
          loading automatically. Reduces network requests and hides your IP from
          preview sources until you choose to load them.
        </p>
        <label className="settings-row">
          <input
            type="checkbox"
            checked={ytEmbeds}
            onChange={(e) => chooseYtEmbeds(e.target.checked)}
          />
          Allow YouTube embeds (sends your IP to YouTube when you watch)
        </label>
        <label className="settings-row">
          <input
            type="checkbox"
            checked={spotifyEmbeds}
            onChange={(e) => chooseSpotifyEmbeds(e.target.checked)}
          />
          Allow Spotify embeds (sends your IP to Spotify when you watch)
        </label>
        <p className="settings-help">
          When off, the first time you click &ldquo;Watch here&rdquo; on a YouTube or
          Spotify card Farder asks before connecting. Turn on to skip that prompt for
          that provider. You can turn it back off here at any time.
        </p>
        <label className="settings-row">
          <input type="checkbox" checked={alwaysFloat} onChange={(e) => chooseAlwaysFloat(e.target.checked)} />
          Always play videos in a floating player (instead of inline)
        </label>
        <label className="settings-row">
          <input type="checkbox" checked={presenceEnabled} onChange={(e) => choosePresenceEnabled(e.target.checked)} />
          Share my activity (let others see what you're doing)
        </label>
        <label className="settings-row">
          <input type="checkbox" checked={presenceMusic} disabled={!presenceEnabled} onChange={(e) => choosePresenceMusic(e.target.checked)} />
          Share music I'm playing
        </label>
        <p className="settings-help">
          Off by default. When on, members on your servers see your current activity
          (e.g. the song you're playing). Turn off any time.
        </p>
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
