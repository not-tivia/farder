// client/src/components/SourceLanguagePicker.tsx

import { useEffect, useState } from "react";
import { downloadModel, listLocalModels } from "../lib/translation/api";
import { displayName } from "../lib/translation/lang";
import type { LocalModel } from "../lib/translation/types";
import { TranslationDownloadDialog } from "./TranslationDownloadDialog";

interface Props {
  /** Currently selected ISO 639-1 source code, or null. */
  value: string | null;
  onChange: (iso1: string) => void;
  /** When set, renders a "Clear override" affordance that calls this. */
  onClear?: () => void;
  /** The target language; we filter installed models to those targeting this. */
  target: string;
  /** Visual mode: a styled <select> (inline use) or a vertical list of menu rows. */
  variant: "select" | "menu";
}

const ADD_NEW_SENTINEL = "__add_new__";
const CLEAR_SENTINEL = "__clear__";

export function SourceLanguagePicker({ value, onChange, onClear, target, variant }: Props) {
  const [installed, setInstalled] = useState<LocalModel[]>([]);
  const [pendingDownload, setPendingDownload] = useState<{ src: string; trg: string; inProgress: boolean } | null>(null);

  async function refresh() {
    try {
      const all = await listLocalModels();
      setInstalled(all.filter((m) => m.pair.trg === target));
    } catch (e) {
      // eslint-disable-next-line no-console
      console.error("[source-picker] failed to list models:", e);
    }
  }

  useEffect(() => {
    refresh();
  }, [target]);

  const installedSources = installed.map((m) => m.pair.src).sort();

  function promptForNewLanguage() {
    // v1.1 stays simple: ask via window.prompt. A future v1.2 can replace
    // this with a full available-pairs picker.
    const iso = window.prompt(
      `Enter an ISO 639-1 code to download (e.g., "ja" for Japanese, "ko" for Korean). Target is ${displayName(target)}.`,
    );
    if (!iso) return;
    const lower = iso.trim().toLowerCase();
    if (!/^[a-z]{2,8}$/.test(lower)) {
      window.alert(`Invalid language code: ${iso}`);
      return;
    }
    setPendingDownload({ src: lower, trg: target, inProgress: false });
  }

  function confirmDownload() {
    if (!pendingDownload) return;
    const pair = { src: pendingDownload.src, trg: pendingDownload.trg };
    setPendingDownload({ ...pendingDownload, inProgress: true });
    (async () => {
      try {
        await downloadModel(pair);
        await refresh();
        onChange(pair.src);
      } catch (e) {
        // eslint-disable-next-line no-console
        console.error("[source-picker] download failed:", e);
      } finally {
        setPendingDownload(null);
      }
    })();
  }

  function cancelDownload() {
    setPendingDownload(null);
  }

  const downloadDialog = pendingDownload && (
    <TranslationDownloadDialog
      pair={{ src: pendingDownload.src, trg: pendingDownload.trg }}
      inProgress={pendingDownload.inProgress}
      onCancel={cancelDownload}
      onConfirm={confirmDownload}
    />
  );

  if (variant === "select") {
    return (
      <>
        <select
          value={value ?? ""}
          onChange={(e) => {
            const v = e.target.value;
            if (v === ADD_NEW_SENTINEL) {
              promptForNewLanguage();
              return;
            }
            if (v === CLEAR_SENTINEL) {
              onClear?.();
              return;
            }
            if (v) onChange(v);
          }}
          style={{
            padding: "4px 8px",
            fontSize: 12,
            background: "var(--xp-window-bg, #ECE9D8)",
            color: "inherit",
            border: "1px solid var(--xp-border, #ACA899)",
            borderRadius: 4,
          }}
        >
          {value === null && <option value="">— pick a language —</option>}
          {installedSources.map((iso) => (
            <option key={iso} value={iso}>
              {displayName(iso)}
            </option>
          ))}
          {(installedSources.length > 0) && <option disabled>──────</option>}
          {onClear && value && (
            <option value={CLEAR_SENTINEL}>Clear override</option>
          )}
          <option value={ADD_NEW_SENTINEL}>Add a new language…</option>
        </select>
        {downloadDialog}
      </>
    );
  }

  // variant === "menu"
  return (
    <div style={{ minWidth: 180 }}>
      {installedSources.length === 0 && (
        <div style={{ padding: "6px 12px", color: "var(--xp-text-muted, #888880)", fontSize: 12 }}>
          No source languages installed.
        </div>
      )}
      {installedSources.map((iso) => {
        const isSelected = iso === value;
        return (
          <div
            key={iso}
            onClick={() => onChange(iso)}
            style={{
              padding: "6px 12px",
              cursor: "pointer",
              display: "flex",
              alignItems: "center",
              gap: 8,
            }}
            onMouseEnter={(e) => (e.currentTarget.style.background = "color-mix(in srgb, var(--xp-blue, #0058E6) 12%, transparent)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            <span style={{ width: 12 }}>{isSelected ? "✓" : ""}</span>
            <span>{displayName(iso)}</span>
          </div>
        );
      })}
      <div style={{ height: 1, background: "var(--xp-border, #ACA899)", margin: "4px 0" }} />
      {onClear && value && (
        <div
          onClick={onClear}
          style={{ padding: "6px 12px", cursor: "pointer", fontSize: 12, color: "var(--xp-text-muted, #888880)" }}
          onMouseEnter={(e) => (e.currentTarget.style.background = "color-mix(in srgb, var(--xp-blue, #0058E6) 12%, transparent)")}
          onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
        >
          Clear override
        </div>
      )}
      <div
        onClick={promptForNewLanguage}
        style={{ padding: "6px 12px", cursor: "pointer", fontSize: 12, color: "var(--xp-text-muted, #888880)" }}
        onMouseEnter={(e) => (e.currentTarget.style.background = "color-mix(in srgb, var(--xp-blue, #0058E6) 12%, transparent)")}
        onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
      >
        Add a new language…
      </div>
      {downloadDialog}
    </div>
  );
}
