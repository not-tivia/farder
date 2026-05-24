// client/src/components/TranslationFirstRunModal.tsx

import { useEffect, useState } from "react";
import {
  getTranslationSettings,
  setTranslationSettings,
  downloadModel,
} from "../lib/translation/api";
import { displayName, ISO_1_TO_3 } from "../lib/translation/lang";

const TARGET_OFFER = ["en", "es", "zh", "fr", "de", "pt", "ja", "ko", "ru", "ar"];
const SOURCE_OFFER = ["en", "es", "zh", "fr", "de", "pt", "ja", "ko", "ru", "ar", "it", "nl", "pl"];

type Step = "target" | "sources";

export function TranslationFirstRunModal() {
  const [show, setShow] = useState(false);
  const [step, setStep] = useState<Step>("target");
  const [target, setTarget] = useState<string>("en");
  const [sources, setSources] = useState<Set<string>>(new Set());
  const [downloading, setDownloading] = useState<string | null>(null);

  useEffect(() => {
    getTranslationSettings().then((s) => {
      if (!s.seen_first_run) {
        setShow(true);
        // Pre-seed target from any existing default; falls back to "en".
        setTarget(s.default_target || "en");
      }
    });
  }, []);

  async function dismiss(): Promise<void> {
    const s = await getTranslationSettings();
    await setTranslationSettings({ ...s, seen_first_run: true });
    setShow(false);
  }

  async function commitTarget(): Promise<void> {
    const s = await getTranslationSettings();
    await setTranslationSettings({ ...s, default_target: target });
    setStep("sources");
  }

  async function downloadSelectedSources(): Promise<void> {
    // For each picked source language, download src → target.
    // (We deliberately do NOT also download target → src — the first-run
    // download is for "translate what I receive into my language"; if the user
    // wants to compose messages and have them translated into other languages,
    // they can add those models from Settings later.)
    for (const src of sources) {
      if (src === target) continue;
      setDownloading(`${src}-${target}`);
      try {
        await downloadModel({ src, trg: target });
      } catch (e) {
        console.error("download failed", { src, trg: target }, e);
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

        {step === "target" && (
          <>
            <p>
              Farder can translate messages between languages, on your device —
              no chat content is ever sent anywhere.
            </p>
            <p style={{ marginTop: 16, fontWeight: 600 }}>
              What language do you speak?
            </p>
            <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
              Foreign-language messages will be translated INTO this language.
            </p>
            <select
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              style={{
                width: "100%",
                padding: "8px 12px",
                fontSize: 14,
                marginTop: 8,
                background: "var(--bg, #fff)",
                color: "var(--text, #000)",
                border: "1px solid var(--border, #ccc)",
                borderRadius: 4,
              }}
            >
              {TARGET_OFFER.map((iso) => (
                <option key={iso} value={iso}>{displayName(iso)}</option>
              ))}
              {/* Allow any iso1 the lang map knows about as a fallback */}
              {Object.keys(ISO_1_TO_3).filter((i) => !TARGET_OFFER.includes(i)).map((iso) => (
                <option key={iso} value={iso}>{displayName(iso)}</option>
              ))}
            </select>
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 16 }}>
              <button onClick={dismiss}>
                Skip — I don't need translation right now
              </button>
              <button onClick={commitTarget}>
                Next →
              </button>
            </div>
          </>
        )}

        {step === "sources" && (
          <>
            <p style={{ fontWeight: 600 }}>
              Which languages should Farder learn to translate FROM?
            </p>
            <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
              Each is ~50 MB and is downloaded from Mozilla's servers.
              You can add or remove languages later in Settings → Translation.
            </p>
            <div style={{ maxHeight: 200, overflowY: "auto", marginTop: 8, border: "1px solid var(--border, #ccc)", borderRadius: 4, padding: 8 }}>
              {SOURCE_OFFER.filter((iso) => iso !== target).map((iso) => (
                <label key={iso} style={{ display: "block", margin: "4px 0" }}>
                  <input
                    type="checkbox"
                    checked={sources.has(iso)}
                    onChange={(e) => {
                      const next = new Set(sources);
                      if (e.target.checked) next.add(iso); else next.delete(iso);
                      setSources(next);
                    }}
                  />
                  {" "}{displayName(iso)}
                </label>
              ))}
            </div>
            {downloading && (
              <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)", marginTop: 8 }}>
                Downloading {downloading}…
              </p>
            )}
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 16 }}>
              <button onClick={() => setStep("target")} disabled={downloading !== null}>
                ← Back
              </button>
              <button onClick={dismiss} disabled={downloading !== null}>
                Skip downloads
              </button>
              <button onClick={downloadSelectedSources} disabled={sources.size === 0 || downloading !== null}>
                Download {sources.size > 0 ? `${sources.size} language${sources.size === 1 ? "" : "s"}` : ""}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
