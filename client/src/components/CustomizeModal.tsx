import { useEffect, useMemo, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import { REGIONS } from "../lib/customizer/regions";
import type { RegionId, RegionState } from "../lib/customizer/types";
import {
  initHistory,
  pushSnapshot,
  undo as histUndo,
  redo as histRedo,
  current as histCurrent,
  canUndo,
  canRedo,
  type HistoryState,
} from "../lib/customizer/history";
import { generateOverrideCss, mergeForSave } from "../lib/customizer/cssGenerator";
import CustomizerRegionRow from "./CustomizerRegionRow";
import CustomizerIntro from "./CustomizerIntro";

interface Props {
  themeId: string;
  initialName: string;
  onClose: () => void;
  onSaved: () => void;
}

const OVERRIDE_STYLE_ID = "active-theme-overrides";
const INTRO_DISMISSED_KEY = "customizerIntroDismissed";

function extractSwatchesFromActiveTheme(): string[] {
  const styleEl = document.getElementById("active-theme");
  if (!styleEl) return [];
  const css = styleEl.textContent ?? "";
  const colors: string[] = [];
  const seen = new Set<string>();
  const re = /--[\w-]+:\s*(#[0-9a-fA-F]{3,8}|rgb[a]?\([^)]+\)|hsl[a]?\([^)]+\))\s*;/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(css)) !== null && colors.length < 12) {
    const c = match[1].trim();
    if (!seen.has(c)) {
      seen.add(c);
      colors.push(c);
    }
  }
  return colors;
}

function setOverrideCss(css: string): void {
  let el = document.getElementById(OVERRIDE_STYLE_ID) as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement("style");
    el.id = OVERRIDE_STYLE_ID;
    document.head.appendChild(el);
  }
  el.textContent = css;
}

function clearOverrideCss(): void {
  const el = document.getElementById(OVERRIDE_STYLE_ID);
  if (el) el.remove();
}

const headerBtn: CSSProperties = {
  font: "inherit",
  padding: "4px 12px",
  background: "var(--xp-panel-bg, #f0ece0)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  borderRadius: 4,
  cursor: "pointer",
};

export default function CustomizeModal({ themeId, initialName, onClose, onSaved }: Props) {
  const [history, setHistory] = useState<HistoryState>(() => initHistory(new Map()));
  const [error, setError] = useState<string | null>(null);
  const [showIntro, setShowIntro] = useState<boolean>(() => localStorage.getItem(INTRO_DISMISSED_KEY) !== "true");
  const [dirty, setDirty] = useState<boolean>(false);
  const swatches = useMemo(() => extractSwatchesFromActiveTheme(), []);

  const regions = useMemo(() => histCurrent(history), [history]);

  function dismissIntro() {
    localStorage.setItem(INTRO_DISMISSED_KEY, "true");
    setShowIntro(false);
  }

  // Apply live preview on every regions change.
  useEffect(() => {
    setOverrideCss(generateOverrideCss(regions));
  }, [regions]);

  // Cleanup override element when this modal unmounts (covers Esc, parent unmount, etc).
  useEffect(() => {
    return () => {
      clearOverrideCss();
    };
  }, []);

  // Keyboard shortcuts: Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z / Ctrl+S
  useEffect(() => {
    function handle(e: KeyboardEvent) {
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key === "z" && !e.shiftKey) {
        e.preventDefault();
        setHistory((h) => histUndo(h));
      } else if (e.key === "y" || (e.key === "z" && e.shiftKey)) {
        e.preventDefault();
        setHistory((h) => histRedo(h));
      } else if (e.key === "s") {
        e.preventDefault();
        void doSave();
      }
    }
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function updateRegion(id: RegionId, next: RegionState | undefined): void {
    const nextMap = new Map(regions);
    if (next === undefined) nextMap.delete(id);
    else nextMap.set(id, next);
    setHistory((h) => pushSnapshot(h, nextMap));
    setDirty(true);
  }

  async function doSave(): Promise<void> {
    try {
      const baseCss = await api.loadThemeCss(themeId);
      const overrides = generateOverrideCss(regions);
      const merged = mergeForSave(baseCss, overrides);
      await api.saveUserTheme(themeId, merged);

      // Refresh the active style to reflect the saved version, drop the override layer.
      const refreshed = await api.loadThemeCss(themeId);
      const activeStyle = document.getElementById("active-theme") as HTMLStyleElement | null;
      if (activeStyle) activeStyle.textContent = refreshed;
      clearOverrideCss();

      setDirty(false);
      onSaved();
    } catch (e) {
      setError(String(e));
    }
  }

  function handleClose(): void {
    if (dirty) {
      const ok = window.confirm("Discard unsaved changes?");
      if (!ok) return;
    }
    clearOverrideCss();
    onClose();
  }

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.4)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1500,
      }}
      onClick={handleClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: "var(--xp-window-bg, #ECE9D8)",
          color: "var(--xp-text-normal, #000)",
          border: "2px solid var(--xp-blue-dark, #003C74)",
          borderRadius: "6px 6px 0 0",
          width: 820,
          maxWidth: "94vw",
          maxHeight: "88vh",
          display: "flex",
          flexDirection: "column",
          fontFamily: "var(--xp-font, Tahoma, sans-serif)",
          fontSize: "var(--xp-font-size, 11px)",
          boxShadow: "3px 3px 16px rgba(0,0,0,0.45)",
          overflow: "hidden",
        }}
      >
        {/* Header */}
        <div
          style={{
            background:
              "linear-gradient(to bottom, var(--xp-blue, #0058E6), var(--xp-blue-light, #3389FF))",
            color: "#fff",
            padding: "4px 8px 4px 12px",
            fontWeight: "bold",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            gap: 8,
          }}
        >
          <span>Customize: {initialName}</span>
          <div style={{ display: "flex", gap: 6 }}>
            <button
              style={headerBtn}
              disabled={!canUndo(history)}
              onClick={() => setHistory((h) => histUndo(h))}
              title="Undo (Ctrl+Z)"
            >
              Undo
            </button>
            <button
              style={headerBtn}
              disabled={!canRedo(history)}
              onClick={() => setHistory((h) => histRedo(h))}
              title="Redo (Ctrl+Y)"
            >
              Redo
            </button>
            <button
              style={headerBtn}
              disabled={!dirty}
              onClick={() => void doSave()}
              title="Save (Ctrl+S)"
            >
              Save
            </button>
            <button
              onClick={handleClose}
              style={{ ...headerBtn, background: "linear-gradient(to bottom, #ee5a5a, #c83030)", color: "#fff", border: "1px solid #fff" }}
              title="Close"
            >
              ✕
            </button>
          </div>
        </div>

        {/* Body */}
        <div style={{ padding: 12, overflowY: "auto", overflowX: "hidden", flex: 1 }}>
          {error && (
            <div style={{ color: "#a00", background: "#fff5f5", border: "1px solid #f3b8b8", padding: 8, marginBottom: 8 }}>
              {error}
            </div>
          )}
          {!dirty && (
            <div
              style={{
                fontSize: 11,
                color: "var(--xp-text-muted, #666)",
                padding: "4px 0 12px",
              }}
            >
              Tip: click any color or image below to start. Use Undo if you change your mind.
            </div>
          )}
          {REGIONS.map((r) => (
            <CustomizerRegionRow
              key={r.id}
              region={r}
              state={regions.get(r.id)}
              themeId={themeId}
              themeSwatches={swatches}
              onChange={(next) => updateRegion(r.id, next)}
              onError={setError}
            />
          ))}
        </div>
      </div>
      {showIntro && <CustomizerIntro onDismiss={dismissIntro} />}
    </div>
  );
}
