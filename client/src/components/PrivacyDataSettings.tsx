import { useEffect, useState } from "react";
import {
  getPresenceEnabled,
  setPresenceEnabled,
  getPresenceMusic,
  setPresenceMusic,
} from "../lib/tauri-bridge";
import { useDataSaver } from "../context/DataSaverContext";
import { getEmbedConsent, setEmbedConsent } from "../lib/embedPlayer";
import { getAlwaysFloat, setAlwaysFloat } from "../lib/floatAnchor";
import SettingsSection from "./settings/SettingsSection";

export default function PrivacyDataSettings() {
  const [ytEmbeds, setYtEmbeds] = useState<boolean>(false);
  const [spotifyEmbeds, setSpotifyEmbeds] = useState<boolean>(false);
  const [alwaysFloat, setAlwaysFloatState] = useState<boolean>(false);
  const [presenceEnabled, setPresenceEnabledState] = useState<boolean>(false);
  const [presenceMusic, setPresenceMusicState] = useState<boolean>(false);
  const { settings: ds, update: updateDs } = useDataSaver();

  useEffect(() => {
    void getPresenceEnabled().then(setPresenceEnabledState).catch(() => {});
    void getPresenceMusic().then(setPresenceMusicState).catch(() => {});
  }, []);

  useEffect(() => {
    setYtEmbeds(getEmbedConsent("youtube"));
    setSpotifyEmbeds(getEmbedConsent("spotify"));
  }, []);

  useEffect(() => { setAlwaysFloatState(getAlwaysFloat()); }, []);

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
    void setPresenceEnabled(enabled).catch((e) => console.error("[privacy-settings] failed to save presence-enabled:", e));
    // If master is turned off, also clear music.
    if (!enabled) {
      setPresenceMusicState(false);
      void setPresenceMusic(false).catch(() => {});
    }
  };
  const choosePresenceMusic = (enabled: boolean) => {
    setPresenceMusicState(enabled);
    void setPresenceMusic(enabled).catch((e) => console.error("[privacy-settings] failed to save presence-music:", e));
  };

  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">Privacy &amp; Data</h2>

      <SettingsSection label="Data Saver">
        <label className="settings-row">
          <input
            type="checkbox"
            checked={ds.enabled}
            onChange={(e) => updateDs({ enabled: e.target.checked })}
          />
          Data Saver
        </label>
        <div style={{ marginLeft: 22, opacity: ds.enabled ? 1 : 0.5 }}>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={ds.gateImages}
              disabled={!ds.enabled}
              onChange={(e) => updateDs({ gateImages: e.target.checked })}
            />
            Don&rsquo;t auto-load large images
          </label>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={ds.clickToLoadEmbeds}
              disabled={!ds.enabled}
              onChange={(e) => updateDs({ clickToLoadEmbeds: e.target.checked })}
            />
            Click-to-load link previews
          </label>
          <label className="settings-row">
            <input
              type="checkbox"
              checked={ds.freezeAvatars}
              disabled={!ds.enabled}
              onChange={(e) => updateDs({ freezeAvatars: e.target.checked })}
            />
            Freeze animated avatars
          </label>
          <label className="settings-row">
            Auto-load media up to&nbsp;
            <input
              type="number"
              min={0}
              step={0.5}
              value={ds.thresholdMB}
              disabled={!ds.enabled || !ds.gateImages}
              onChange={(e) => updateDs({ thresholdMB: Math.max(0, parseFloat(e.target.value) || 0) })}
              style={{ width: 56 }}
            />
            &nbsp;MB
          </label>
        </div>
        <p className="settings-help">
          When on, large images show a &ldquo;Load image&rdquo; button instead of
          downloading automatically, link previews wait for a click, and animated
          avatars are shown as a still frame. Small files load normally.
        </p>
      </SettingsSection>

      <div className="settings-divider" />
      <SettingsSection label="Embeds &amp; Players">
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
      </SettingsSection>

      <div className="settings-divider" />
      <SettingsSection label="Activity">
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
    </div>
  );
}
