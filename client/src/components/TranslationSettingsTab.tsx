// client/src/components/TranslationSettingsTab.tsx

import { useEffect, useState } from "react";
import {
  getTranslationSettings,
  setTranslationSettings,
  listLocalModels,
  listAvailablePairs,
  deleteModel,
  downloadModel,
} from "../lib/translation/api";
import { displayName, ISO_1_TO_3 } from "../lib/translation/lang";
import type { LocalModel, AvailablePair, TranslationSettings } from "../lib/translation/types";

export function TranslationSettingsTab() {
  const [settings, setSettings] = useState<TranslationSettings | null>(null);
  const [installed, setInstalled] = useState<LocalModel[]>([]);
  const [available, setAvailable] = useState<AvailablePair[]>([]);
  const [showAdd, setShowAdd] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  async function refresh() {
    setSettings(await getTranslationSettings());
    setInstalled(await listLocalModels());
    if (showAdd) setAvailable(await listAvailablePairs());
  }

  useEffect(() => { refresh(); }, [showAdd]);

  if (!settings) return <div>Loading…</div>;

  return (
    <div style={{ padding: 16 }}>
      <h2>Translation</h2>

      <label style={{ display: "block", margin: "12px 0" }}>
        <input
          type="checkbox"
          checked={settings.enabled}
          onChange={async (e) => {
            const next = { ...settings, enabled: e.target.checked };
            await setTranslationSettings(next);
            setSettings(next);
          }}
        />
        {" "}Enable translation
      </label>

      <label style={{ display: "block", margin: "12px 0" }}>
        Default target language:{" "}
        <select
          value={settings.default_target}
          onChange={async (e) => {
            const next = { ...settings, default_target: e.target.value };
            await setTranslationSettings(next);
            setSettings(next);
          }}
        >
          {Object.keys(ISO_1_TO_3).map((iso) => (
            <option key={iso} value={iso}>{displayName(iso)}</option>
          ))}
        </select>
      </label>

      <h3 style={{ marginTop: 24 }}>Installed languages</h3>
      {installed.length === 0 && <p>No models installed yet.</p>}
      <ul>
        {installed.map((m) => (
          <li key={`${m.pair.src}-${m.pair.trg}`} style={{ margin: "6px 0" }}>
            {displayName(m.pair.src)} → {displayName(m.pair.trg)}
            {" "}({(m.disk_size_bytes / 1_000_000).toFixed(1)} MB)
            {" "}
            <button
              disabled={busy !== null}
              onClick={async () => {
                if (!confirm(`Delete ${displayName(m.pair.src)}→${displayName(m.pair.trg)} model?`)) return;
                setBusy("deleting");
                try { await deleteModel(m.pair); await refresh(); }
                finally { setBusy(null); }
              }}
            >Delete</button>
          </li>
        ))}
      </ul>

      <button onClick={() => setShowAdd(!showAdd)}>
        {showAdd ? "Hide available languages" : "+ Add language"}
      </button>

      {showAdd && (
        <ul style={{ maxHeight: 240, overflowY: "auto", border: "1px solid var(--border, #ccc)", padding: 8, marginTop: 8 }}>
          {available
            .filter((p) =>
              !installed.some((m) => m.pair.src === p.src && m.pair.trg === p.trg)
            )
            .map((p) => (
              <li key={`${p.src}-${p.trg}`} style={{ margin: "4px 0" }}>
                {displayName(p.src)} → {displayName(p.trg)}
                {" "}({(p.size_bytes / 1_000_000).toFixed(1)} MB)
                {" "}
                <button
                  disabled={busy !== null}
                  onClick={async () => {
                    setBusy(`downloading-${p.src}-${p.trg}`);
                    try { await downloadModel({ src: p.src, trg: p.trg }); await refresh(); }
                    catch (e) { alert(`Download failed: ${e}`); }
                    finally { setBusy(null); }
                  }}
                >
                  {busy === `downloading-${p.src}-${p.trg}` ? "Downloading…" : "Download"}
                </button>
              </li>
            ))}
        </ul>
      )}
    </div>
  );
}
