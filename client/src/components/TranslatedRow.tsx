// client/src/components/TranslatedRow.tsx

import { useEffect, useState } from "react";
import { subscribe, dismiss, translateMessageWithSource } from "../lib/translation/store";
import { getTranslationSettings, setTranslationSettings } from "../lib/translation/api";
import { displayName } from "../lib/translation/lang";
import type { TranslationStatus } from "../lib/translation/types";
import { SourceLanguagePicker } from "./SourceLanguagePicker";

interface Props {
  messageId: string;
  content: string;
  defaultTarget: string;
  authorPublicKeyHex: string;
  confirmDownload: (pair: { src: string; trg: string }) => Promise<void>;
}

export function TranslatedRow({ messageId, content, defaultTarget, authorPublicKeyHex, confirmDownload }: Props) {
  const [status, setStatus] = useState<TranslationStatus>({ kind: "idle" });
  const [overrideForAuthor, setOverrideForAuthor] = useState<string | null>(null);

  useEffect(() => {
    const unsub = subscribe((m) => {
      setStatus(m.get(messageId) ?? { kind: "idle" });
    });
    return unsub;
  }, [messageId]);

  // Load the current override for this author (so we can decide whether to
  // show the "Always translate…" button). Re-read after status changes too,
  // in case the user just clicked it.
  useEffect(() => {
    if (!authorPublicKeyHex) return;
    getTranslationSettings()
      .then((s) => {
        setOverrideForAuthor(s.user_language_overrides[authorPublicKeyHex] ?? null);
      })
      .catch(() => {});
  }, [authorPublicKeyHex, status.kind]);

  async function setAlwaysTranslate(src: string) {
    try {
      const settings = await getTranslationSettings();
      await setTranslationSettings({
        ...settings,
        user_language_overrides: {
          ...settings.user_language_overrides,
          [authorPublicKeyHex]: src,
        },
      });
      setOverrideForAuthor(src);
    } catch (e) {
      // eslint-disable-next-line no-console
      console.error("[translated-row] set always-translate failed:", e);
    }
  }

  if (status.kind === "idle") return null;

  return (
    <div
      className="translated-row"
      style={{
        marginTop: 4,
        padding: "4px 8px",
        fontSize: "0.9em",
        color: "var(--text-muted, #888)",
        borderLeft: "2px solid var(--accent, #888)",
        display: "flex",
        alignItems: "flex-start",
        gap: 8,
      }}
    >
      <div style={{ flex: 1 }}>
        {status.kind === "detecting" && <span>Detecting language…</span>}
        {status.kind === "loading-model" && (
          <span>Loading {displayName(status.src)} → {displayName(status.trg)} model…</span>
        )}
        {status.kind === "downloading-model" && (
          <span>
            Downloading {displayName(status.src)} model ({Math.round(status.progress.bytes_done / 1_000_000)}{" "}
            of {Math.round(status.progress.bytes_total / 1_000_000)} MB)…
          </span>
        )}
        {status.kind === "translating" && <span>Translating…</span>}
        {status.kind === "done" && (
          <>
            <div>{status.text}</div>
            <div style={{ fontSize: "0.8em", marginTop: 2, display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
              <span>↳ Translated from {displayName(status.src)}</span>
              {overrideForAuthor !== status.src && authorPublicKeyHex && (
                <button
                  onClick={() => void setAlwaysTranslate(status.src)}
                  style={{
                    background: "none",
                    border: "1px solid var(--border, #ccc)",
                    borderRadius: 3,
                    color: "inherit",
                    cursor: "pointer",
                    fontSize: "inherit",
                    padding: "1px 6px",
                  }}
                  title={`Auto-translate every future message from this user from ${displayName(status.src)}.`}
                >
                  Always translate {displayName(status.src)} for this user
                </button>
              )}
            </div>
          </>
        )}
        {status.kind === "already-in-target" && (
          <span>Already in {displayName(status.lang)}</span>
        )}
        {status.kind === "low-confidence" && (
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <span>Couldn't detect language. Source:</span>
            <SourceLanguagePicker
              variant="select"
              target={defaultTarget}
              value={status.suggested ?? null}
              onChange={(src) =>
                translateMessageWithSource({
                  messageId,
                  content,
                  src,
                  defaultTarget,
                  authorPublicKeyHex,
                  confirmDownload,
                })
              }
            />
          </div>
        )}
        {status.kind === "error" && (
          <span style={{ color: "var(--error, #c44)" }}>Translation failed: {status.reason}</span>
        )}
      </div>
      <button
        onClick={() => dismiss(messageId)}
        style={{ background: "none", border: "none", cursor: "pointer", color: "inherit" }}
        title="Dismiss translation"
      >
        ×
      </button>
    </div>
  );
}
