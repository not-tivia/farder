import { useEffect, useState } from "react";
import {
  getPresenceEnabled,
  setPresenceEnabled,
  getPresenceMusic,
  setPresenceMusic,
  listBlocked,
  unblockUser,
  getDeletionStatus,
  requestDeletion,
  cancelDeletion,
} from "../lib/tauri-bridge";
import type { BlockedUserInfo, DeletionStatus } from "../lib/tauri-bridge";
import { useActiveServerId } from "../context/ServerContext";
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
  // Blocking is per-server (the block lives in that server's database), so the
  // list is the active server's.
  const activeServerId = useActiveServerId();
  const [blocked, setBlocked] = useState<BlockedUserInfo[] | null>(null);
  const [blockedError, setBlockedError] = useState<string | null>(null);

  useEffect(() => {
    if (!activeServerId) {
      setBlocked(null);
      return;
    }
    let cancelled = false;
    listBlocked(activeServerId)
      .then((list) => { if (!cancelled) { setBlocked(list); setBlockedError(null); } })
      .catch((e) => { if (!cancelled) setBlockedError(String(e)); });
    return () => { cancelled = true; };
  }, [activeServerId]);

  // "Delete my data on this server": the server has had the whole flow --
  // request, 72-hour grace period, cancel, and a sweep that anonymizes the
  // messages and removes the membership -- with no way to reach any of it.
  const [deletion, setDeletion] = useState<DeletionStatus | null>(null);
  const [deletionError, setDeletionError] = useState<string | null>(null);

  useEffect(() => {
    if (!activeServerId) {
      setDeletion(null);
      return;
    }
    let cancelled = false;
    getDeletionStatus(activeServerId)
      .then((s) => { if (!cancelled) { setDeletion(s); setDeletionError(null); } })
      .catch((e) => { if (!cancelled) setDeletionError(String(e)); });
    return () => { cancelled = true; };
  }, [activeServerId]);

  async function refreshDeletion(serverId: string) {
    setDeletion(await getDeletionStatus(serverId));
  }

  async function handleRequestDeletion() {
    if (!activeServerId) return;
    // A destructive, delayed action deserves a plain-language confirmation of
    // what it actually does -- not "are you sure?".
    const ok = window.confirm(
      "Request deletion of your data on this server?\n\n" +
      "After 72 hours the server removes your membership, anonymizes every message " +
      "you posted and deletes the files you uploaded. Nothing happens before then, " +
      "and you can cancel at any point during those 72 hours.",
    );
    if (!ok) return;
    try {
      await requestDeletion(activeServerId);
      await refreshDeletion(activeServerId);
      setDeletionError(null);
    } catch (e) {
      setDeletionError(String(e));
    }
  }

  async function handleCancelDeletion() {
    if (!activeServerId) return;
    try {
      await cancelDeletion(activeServerId);
      await refreshDeletion(activeServerId);
      setDeletionError(null);
    } catch (e) {
      setDeletionError(String(e));
    }
  }

  async function handleUnblock(publicKey: string) {
    if (!activeServerId) return;
    try {
      await unblockUser(activeServerId, publicKey);
      // Re-read rather than splicing locally: the server is the authority on
      // what is blocked, and a failed unblock must not look like a successful
      // one.
      setBlocked(await listBlocked(activeServerId));
      setBlockedError(null);
    } catch (e) {
      setBlockedError(String(e));
    }
  }

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
      <SettingsSection label="Large Files">
        <label className="settings-row">
          Always ask before downloading files over&nbsp;
          <input
            type="number"
            min={0}
            step={1}
            value={ds.askAboveMB}
            onChange={(e) => updateDs({ askAboveMB: Math.max(0, parseFloat(e.target.value) || 0) })}
            style={{ width: 64 }}
          />
          &nbsp;MB
        </label>
        <p className="settings-help">
          This one applies whether Data Saver is on or off, and to every kind of
          file. Anything larger shows its name and size with an
          &ldquo;Accept and download&rdquo; button, and nothing crosses your
          connection until you press it &mdash; so a 2&nbsp;GB clip someone drops
          in a channel cannot start arriving because you scrolled past it. Set it
          to 0 to be asked about everything.
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

      <div className="settings-divider" />
      <SettingsSection label="Blocked Members">
        {!activeServerId && (
          <p className="settings-help">
            Blocking is per server. Connect to a server to see and undo the blocks you
            made there.
          </p>
        )}
        {blockedError && <div className="error-text">{blockedError}</div>}
        {activeServerId && blocked !== null && blocked.length === 0 && (
          <p className="settings-help">You haven't blocked anyone on this server.</p>
        )}
        {activeServerId && blocked === null && !blockedError && (
          <p className="settings-help">Loading...</p>
        )}
        {(blocked ?? []).map((b) => (
          <div key={b.public_key} className="organizer-row">
            <span className="organizer-name">
              {b.display_name ?? `${b.public_key.slice(0, 12)}...`}
            </span>
            <button className="xp-button" onClick={() => { void handleUnblock(b.public_key); }}>
              Unblock
            </button>
          </div>
        ))}
        <p className="settings-help">
          A blocked member cannot DM you and you cannot DM them. They are not told,
          and this list is only ever your own.
        </p>
      </SettingsSection>

      <div className="settings-divider" />
      <SettingsSection label="Delete My Data">
        {!activeServerId && (
          <p className="settings-help">
            Connect to a server to request deletion of your data there.
          </p>
        )}
        {deletionError && <div className="error-text">{deletionError}</div>}
        {activeServerId && deletion?.pending && (
          <>
            <p className="settings-help">
              Deletion is scheduled for{" "}
              <strong>
                {deletion.expires_at
                  ? new Date(deletion.expires_at * 1000).toLocaleString()
                  : "soon"}
              </strong>
              . Until then nothing has been removed, and cancelling undoes it entirely.
            </p>
            <button className="xp-button" onClick={() => { void handleCancelDeletion(); }}>
              Cancel deletion
            </button>
          </>
        )}
        {activeServerId && deletion && !deletion.pending && (
          <>
            <button className="xp-button" onClick={() => { void handleRequestDeletion(); }}>
              Request deletion of my data
            </button>
            <p className="settings-help">
              After a 72-hour grace period the server removes your membership,
              anonymizes your messages and deletes your uploaded files. Other members'
              copies of what you wrote are not affected -- nothing can reach those.
              The server owner cannot request this; ownership has to be transferred
              first.
            </p>
          </>
        )}
      </SettingsSection>
    </div>
  );
}
