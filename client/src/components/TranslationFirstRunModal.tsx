// client/src/components/TranslationFirstRunModal.tsx

import { useEffect, useState } from "react";
import {
  getTranslationSettings,
  setTranslationSettings,
  downloadModel,
} from "../lib/translation/api";
import { displayName } from "../lib/translation/lang";

const DEFAULT_OFFER = ["en", "es", "zh"];

export function TranslationFirstRunModal() {
  const [show, setShow] = useState(false);
  const [picks, setPicks] = useState<Set<string>>(new Set());
  const [downloading, setDownloading] = useState<string | null>(null);

  useEffect(() => {
    getTranslationSettings().then((s) => {
      if (!s.seen_first_run) setShow(true);
    });
  }, []);

  async function dismiss(): Promise<void> {
    const s = await getTranslationSettings();
    await setTranslationSettings({ ...s, seen_first_run: true });
    setShow(false);
  }

  async function downloadSelected(): Promise<void> {
    // Build pairs: each picked language ↔ defaultTarget. Default target is the first
    // picked or "en". For v1, we treat the first picked as the user's primary language.
    const target = Array.from(picks)[0] ?? "en";
    const others = Array.from(picks).filter((p) => p !== target);

    const s = await getTranslationSettings();
    await setTranslationSettings({ ...s, default_target: target });

    for (const other of others) {
      // Both directions
      for (const pair of [{ src: other, trg: target }, { src: target, trg: other }]) {
        setDownloading(`${pair.src}-${pair.trg}`);
        try { await downloadModel(pair); }
        catch (e) { console.error("download failed", pair, e); }
      }
    }
    setDownloading(null);
    await dismiss();
  }

  if (!show) return null;

  return (
    <div className="modal-overlay" style={{
      position: "fixed", inset: 0, background: "rgba(0,0,0,0.5)",
      display: "flex", alignItems: "center", justifyContent: "center", zIndex: 1100,
    }}>
      <div className="modal" style={{
        background: "var(--bg-elevated, #fff)", color: "var(--text, #000)",
        padding: 20, borderRadius: 8, maxWidth: 480, width: "90%",
      }}>
        <h2>Translation</h2>
        <p>
          Farder can translate messages between languages, on your device — no
          chat content is ever sent anywhere.
        </p>
        <p>
          Pick the languages you want to translate to and from. Each is ~50 MB
          and is downloaded from Mozilla's servers.
        </p>
        {DEFAULT_OFFER.map((iso) => (
          <label key={iso} style={{ display: "block", margin: "6px 0" }}>
            <input
              type="checkbox"
              checked={picks.has(iso)}
              onChange={(e) => {
                const next = new Set(picks);
                if (e.target.checked) next.add(iso); else next.delete(iso);
                setPicks(next);
              }}
            />
            {" "}{displayName(iso)}
          </label>
        ))}
        {downloading && (
          <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
            Downloading {downloading}…
          </p>
        )}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 16 }}>
          <button onClick={dismiss} disabled={downloading !== null}>
            Skip — I don't need translation right now
          </button>
          <button onClick={downloadSelected} disabled={picks.size === 0 || downloading !== null}>
            Download selected
          </button>
        </div>
      </div>
    </div>
  );
}
