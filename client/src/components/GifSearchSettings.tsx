import { useEffect, useState, type CSSProperties } from "react";
import * as gifApi from "../lib/gifSearch";
import type { GifSearchSettings as Settings } from "../lib/gifSearch";

const row: CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  padding: "8px 0",
  gap: 12,
};

const TENOR_DOCS_URL = "https://developers.google.com/tenor/guides/quickstart";

export default function GifSearchSettings() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    gifApi.getGifSearchSettings().then(setSettings).catch((e) => setError(String(e)));
  }, []);

  function update(patch: Partial<Settings>) {
    if (!settings) return;
    const next: Settings = { ...settings, ...patch };
    if (patch.content_filter === "off" && settings.content_filter !== "off") {
      if (!window.confirm("Content filter off — adult content may appear in your searches. Are you sure?")) {
        return;
      }
    }
    setSettings(next);
    gifApi.setGifSearchSettings(next).catch((e) => setError(String(e)));
  }

  if (!settings) {
    return <div style={{ padding: 12 }}>Loading…</div>;
  }

  return (
    <div style={{ padding: 12 }}>
      <h3 style={{ marginTop: 0 }}>GIF Search</h3>
      {error && <div style={{ color: "#a00", marginBottom: 8 }}>{error}</div>}

      <div style={row}>
        <label htmlFor="gif-enabled">Enable Tenor GIF search</label>
        <input
          id="gif-enabled"
          type="checkbox"
          checked={settings.enabled}
          onChange={(e) => update({ enabled: e.target.checked })}
        />
      </div>

      {settings.enabled && (
        <>
          <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)", marginTop: 4 }}>
            Tenor is owned by Google. Searches are sent to Google's servers; your IP and search terms are visible to them.
          </p>

          <div style={row}>
            <label htmlFor="gif-filter">Content filter</label>
            <select
              id="gif-filter"
              value={settings.content_filter}
              onChange={(e) => update({ content_filter: e.target.value as Settings["content_filter"] })}
              style={{ font: "inherit" }}
            >
              <option value="high">High (default)</option>
              <option value="medium">Medium</option>
              <option value="low">Low</option>
              <option value="off">Off</option>
            </select>
          </div>

          <div style={{ marginTop: 12 }}>
            <label htmlFor="gif-key" style={{ display: "block", marginBottom: 4 }}>
              Your Tenor API key (optional)
            </label>
            <input
              id="gif-key"
              type="text"
              placeholder="leave blank to use Farder's default"
              value={settings.user_api_key ?? ""}
              onChange={(e) => update({ user_api_key: e.target.value || null })}
              style={{ width: "100%", font: "inherit", boxSizing: "border-box" }}
            />
            <p style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 4 }}>
              Setting your own key avoids sharing the default Farder quota.{" "}
              <a
                href={TENOR_DOCS_URL}
                onClick={(e) => {
                  e.preventDefault();
                  window.open(TENOR_DOCS_URL, "_blank");
                }}
                style={{ color: "var(--xp-blue, #0058E6)" }}
              >
                How to get a Tenor API key
              </a>
            </p>
          </div>
        </>
      )}
    </div>
  );
}
