import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";

interface Props {
  onClose: () => void;
}

// Pull a few representative colors out of a theme's CSS for the swatch strip.
// Looks for `--<prefix>-bg`, `--<prefix>-blue`, `--<prefix>-accent`, etc, and
// returns up to 5 distinct color values in declaration order.
function extractSwatch(css: string): string[] {
  const colors: string[] = [];
  const seen = new Set<string>();
  const re = /--[\w-]+:\s*(#[0-9a-fA-F]{3,8}|rgb[a]?\([^)]+\)|hsl[a]?\([^)]+\))\s*;/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(css)) !== null && colors.length < 5) {
    const c = match[1].trim();
    if (!seen.has(c)) {
      seen.add(c);
      colors.push(c);
    }
  }
  return colors;
}

export default function AppearanceSettings({ onClose }: Props) {
  const [themes, setThemes] = useState<api.ThemeMeta[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [swatchByCss, setSwatchByCss] = useState<Record<string, string[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setLoading(true);
    setError(null);
    try {
      const list = await api.listThemes();
      setThemes(list);
      const active = await api.getActiveTheme();
      setActiveId(active.id);
      // Load CSS for each to compute swatches. Cheap — already in memory on Rust side.
      const swatches: Record<string, string[]> = {};
      for (const t of list) {
        try {
          const css = await api.loadThemeCss(t.id);
          swatches[t.id] = extractSwatch(css);
        } catch {
          swatches[t.id] = [];
        }
      }
      setSwatchByCss(swatches);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  async function selectTheme(id: string) {
    try {
      const css = await api.loadThemeCss(id);
      const styleEl = document.getElementById("active-theme");
      if (styleEl) styleEl.textContent = css;
      document.documentElement.dataset.theme = id;
      await api.setActiveTheme(id);
      setActiveId(id);
    } catch (e) {
      console.error("[appearance] failed to switch theme:", e);
      setError(String(e));
    }
  }

  return (
    <div
      className="appearance-settings-backdrop"
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.4)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1000,
      }}
      onClick={onClose}
    >
      <div
        className="appearance-settings"
        style={{
          background: "var(--xp-window-bg, #ECE9D8)",
          color: "#000",
          border: "2px solid var(--xp-blue-dark, #003C74)",
          borderRadius: 6,
          width: 560,
          maxHeight: "80vh",
          display: "flex",
          flexDirection: "column",
          fontFamily: "var(--xp-font, Tahoma, sans-serif)",
          fontSize: "var(--xp-font-size, 11px)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          style={{
            background: "linear-gradient(to right, var(--xp-blue, #0058E6), var(--xp-blue-light, #3389FF))",
            color: "#fff",
            padding: "4px 8px",
            fontWeight: "bold",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <span>Appearance</span>
          <button
            onClick={onClose}
            style={{ background: "transparent", color: "#fff", border: "1px solid #fff", padding: "0 6px", cursor: "pointer" }}
            title="Close"
          >
            ✕
          </button>
        </div>

        <div style={{ padding: 12, overflow: "auto", flex: 1 }}>
          {loading && <div>Loading themes…</div>}
          {error && <div style={{ color: "#a00" }}>Error: {error}</div>}
          {!loading && !error && (
            <>
              <div
                style={{
                  display: "grid",
                  gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))",
                  gap: 10,
                }}
              >
                {themes.map((t) => {
                  const isActive = t.id === activeId;
                  const swatch = swatchByCss[t.id] ?? [];
                  return (
                    <button
                      key={t.id}
                      onClick={() => selectTheme(t.id)}
                      style={{
                        textAlign: "left",
                        padding: 10,
                        border: isActive ? "2px solid var(--xp-blue, #0058E6)" : "1px solid #aca899",
                        background: "#fff",
                        cursor: "pointer",
                        display: "flex",
                        flexDirection: "column",
                        gap: 6,
                      }}
                    >
                      <div style={{ fontWeight: "bold" }}>{t.name}</div>
                      <div style={{ fontSize: 10, color: "#555" }}>
                        {t.author} · {t.source}
                      </div>
                      <div style={{ fontSize: 10, color: "#555" }}>{t.description}</div>
                      <div style={{ display: "flex", gap: 2, marginTop: 4 }}>
                        {swatch.map((c, i) => (
                          <div
                            key={i}
                            style={{
                              width: 24,
                              height: 16,
                              background: c,
                              border: "1px solid #888",
                            }}
                          />
                        ))}
                      </div>
                    </button>
                  );
                })}
              </div>

              <div
                style={{
                  marginTop: 14,
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  flexWrap: "wrap",
                }}
              >
                <button onClick={() => api.openThemesFolder().catch((e) => setError(String(e)))}>
                  Open themes folder
                </button>
                <button onClick={refresh} title="Re-scan ~/.farder/themes/">
                  Refresh
                </button>
                <span style={{ fontSize: 10, color: "#666", flex: 1, minWidth: 200 }}>
                  Themes can load external resources. Only use themes from sources you trust.
                </span>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
