import { useEffect, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import CustomizeModal from "./CustomizeModal";
import GifSearchSettings from "./GifSearchSettings";
import { TranslationSettingsTab } from "./TranslationSettingsTab";

interface Props {
  onClose: () => void;
}

// Reorder a theme list according to the user's saved preference.
// Themes named in `order` come first in that order; any unlisted themes
// follow in the order list_themes returned them (Rust sorts alphabetically).
function applyOrder(list: api.ThemeMeta[], order: string[]): api.ThemeMeta[] {
  if (order.length === 0) return list;
  const byId = new Map(list.map((t) => [t.id, t]));
  const used = new Set<string>();
  const out: api.ThemeMeta[] = [];
  for (const id of order) {
    const t = byId.get(id);
    if (t) {
      out.push(t);
      used.add(id);
    }
  }
  for (const t of list) {
    if (!used.has(t.id)) out.push(t);
  }
  return out;
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

const chromeButton: CSSProperties = {
  padding: "4px 14px",
  background: "var(--xp-panel-bg, #f0ece0)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  borderRadius: 4,
  font: "inherit",
  cursor: "pointer",
  whiteSpace: "nowrap",
};

const closeButton: CSSProperties = {
  background: "linear-gradient(to bottom, #ee5a5a 0%, #c83030 100%)",
  color: "#fff",
  border: "1px solid #fff",
  borderRadius: 3,
  width: 22,
  height: 18,
  lineHeight: "16px",
  padding: 0,
  fontWeight: "bold",
  cursor: "pointer",
};

export default function AppearanceSettings({ onClose }: Props) {
  const [themes, setThemes] = useState<api.ThemeMeta[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [swatchByCss, setSwatchByCss] = useState<Record<string, string[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [customizing, setCustomizing] = useState<{ themeId: string; name: string } | null>(null);
  const [activeTab, setActiveTab] = useState<"appearance" | "gif" | "translation">("appearance");

  async function refresh() {
    setLoading(true);
    setError(null);
    try {
      const list = await api.listThemes();
      const order = await api.getThemeOrder();
      const ordered = applyOrder(list, order);
      setThemes(ordered);
      const active = await api.getActiveTheme();
      setActiveId(active.id);
      const swatches: Record<string, string[]> = {};
      for (const t of ordered) {
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

  async function reorderThemes(fromIndex: number, toIndex: number): Promise<void> {
    if (fromIndex === toIndex) return;
    const next = themes.slice();
    const [moved] = next.splice(fromIndex, 1);
    next.splice(toIndex, 0, moved);
    setThemes(next);
    try {
      await api.setThemeOrder(next.map((t) => t.id));
    } catch (e) {
      setError(String(e));
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

  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState<string>("");
  const [dragSrcIndex, setDragSrcIndex] = useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);

  async function deleteTheme(t: api.ThemeMeta): Promise<void> {
    if (!window.confirm(`Delete "${t.name}"? This removes the folder from disk and can't be undone.`)) return;
    try {
      await api.deleteUserTheme(t.id);
      // If we just deleted the active theme, fall back to the first remaining one.
      if (t.id === activeId) {
        const remaining = (await api.listThemes()).find((x) => x.id !== t.id);
        if (remaining) {
          await selectTheme(remaining.id);
        }
      }
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function commitRename(t: api.ThemeMeta): Promise<void> {
    const next = renameDraft.trim();
    setRenamingId(null);
    if (!next || next === t.name) return;
    try {
      await api.renameUserTheme(t.id, next);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function startCustomizing(base: api.ThemeMeta): Promise<void> {
    // User themes: edit in place. Built-ins: fork to a new user theme first
    // (built-ins are compiled in and can't be modified directly).
    if (base.source === "user") {
      setCustomizing({ themeId: base.id, name: base.name });
      return;
    }
    const proposedName = window.prompt(
      `Customize a copy of "${base.name}". Name it:`,
      `${base.name} (Custom)`,
    );
    if (!proposedName) return;
    try {
      const newId = await api.forkTheme(base.id, proposedName.toLowerCase().replace(/\s+/g, "-"), proposedName);
      // Refresh the picker list so the new theme appears, then open the customizer on it.
      await refresh();
      setCustomizing({ themeId: newId, name: proposedName });
    } catch (e) {
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
          borderRadius: "6px 6px 0 0",
          width: 720,
          maxWidth: "92vw",
          maxHeight: "82vh",
          minHeight: 320,
          display: "flex",
          flexDirection: "column",
          fontFamily: "var(--xp-font, Tahoma, sans-serif)",
          fontSize: "var(--xp-font-size, 11px)",
          boxShadow: "3px 3px 16px rgba(0,0,0,0.45)",
          overflow: "hidden",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Title bar */}
        <div
          style={{
            background:
              "linear-gradient(to bottom, var(--xp-blue, #0058E6) 0%, var(--xp-blue-light, #3389FF) 100%)",
            color: "#fff",
            padding: "4px 6px 4px 10px",
            fontWeight: "bold",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            flexShrink: 0,
          }}
        >
          <span>Settings</span>
          <button onClick={onClose} style={closeButton} title="Close">
            ✕
          </button>
        </div>

        {/* Tab bar */}
        <div
          style={{
            display: "flex",
            borderBottom: "1px solid var(--xp-border, #888)",
            padding: "0 4px",
            flexShrink: 0,
            background: "var(--xp-window-bg, #ECE9D8)",
          }}
        >
          {(["appearance", "gif", "translation"] as const).map((tab) => (
            <button
              key={tab}
              onClick={() => setActiveTab(tab)}
              style={{
                font: "inherit",
                padding: "6px 12px",
                background: activeTab === tab ? "var(--xp-panel-bg, #fff)" : "transparent",
                color: activeTab === tab ? "var(--xp-blue, #0058E6)" : "inherit",
                border: "none",
                borderBottom:
                  activeTab === tab
                    ? "2px solid var(--xp-blue, #0058E6)"
                    : "2px solid transparent",
                cursor: "pointer",
              }}
            >
              {tab === "appearance" ? "Appearance" : tab === "gif" ? "GIF Search" : "Translation"}
            </button>
          ))}
        </div>

        {/* Body */}
        <div
          style={{
            padding: 16,
            overflowY: "auto",
            overflowX: "hidden",
            flex: 1,
            display: "flex",
            flexDirection: "column",
            gap: 14,
          }}
        >
          {activeTab === "gif" && <GifSearchSettings />}
          {activeTab === "translation" && <TranslationSettingsTab />}
          {activeTab === "appearance" && (
            <>
          {loading && <div>Loading themes…</div>}
          {error && (
            <div
              style={{
                color: "#a00",
                background: "#fff5f5",
                border: "1px solid #f3b8b8",
                padding: 8,
                borderRadius: 3,
              }}
            >
              {error}
            </div>
          )}
          {!loading && (
            <>
              <div
                style={{
                  display: "grid",
                  gridTemplateColumns: "repeat(auto-fill, minmax(210px, 1fr))",
                  gap: 12,
                }}
              >
                {themes.map((t, idx) => {
                  const isActive = t.id === activeId;
                  const swatch = swatchByCss[t.id] ?? [];
                  const isDragging = dragSrcIndex === idx;
                  const isDropTarget = dragOverIndex === idx && dragSrcIndex !== null && dragSrcIndex !== idx;
                  return (
                    <button
                      key={t.id}
                      onClick={() => selectTheme(t.id)}
                      draggable={renamingId !== t.id}
                      onDragStart={(e) => {
                        setDragSrcIndex(idx);
                        e.dataTransfer.effectAllowed = "move";
                      }}
                      onDragOver={(e) => {
                        e.preventDefault();
                        e.dataTransfer.dropEffect = "move";
                        if (dragOverIndex !== idx) setDragOverIndex(idx);
                      }}
                      onDragLeave={() => {
                        if (dragOverIndex === idx) setDragOverIndex(null);
                      }}
                      onDrop={(e) => {
                        e.preventDefault();
                        if (dragSrcIndex !== null) void reorderThemes(dragSrcIndex, idx);
                        setDragSrcIndex(null);
                        setDragOverIndex(null);
                      }}
                      onDragEnd={() => {
                        setDragSrcIndex(null);
                        setDragOverIndex(null);
                      }}
                      style={{
                        textAlign: "left",
                        padding: 12,
                        border: isDropTarget
                          ? "2px dashed var(--xp-blue, #0058E6)"
                          : isActive
                          ? "2px solid var(--xp-blue, #0058E6)"
                          : "1px solid var(--xp-border, #aca899)",
                        background: "var(--xp-panel-bg, #fff)",
                        cursor: isDragging ? "grabbing" : "pointer",
                        opacity: isDragging ? 0.4 : 1,
                        display: "flex",
                        flexDirection: "column",
                        gap: 6,
                        font: "inherit",
                        color: "var(--xp-text-normal, #000)",
                        boxShadow: isActive ? "0 0 0 1px var(--xp-blue, #0058E6) inset" : "none",
                      }}
                    >
                      <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
                        {renamingId === t.id ? (
                          <input
                            autoFocus
                            value={renameDraft}
                            onChange={(e) => setRenameDraft(e.target.value)}
                            onClick={(e) => e.stopPropagation()}
                            onKeyDown={(e) => {
                              e.stopPropagation();
                              if (e.key === "Enter") void commitRename(t);
                              else if (e.key === "Escape") setRenamingId(null);
                            }}
                            onBlur={() => void commitRename(t)}
                            style={{
                              fontWeight: "bold",
                              fontSize: 12,
                              flex: 1,
                              minWidth: 0,
                              padding: "1px 4px",
                              border: "1px solid var(--xp-blue, #0058E6)",
                              background: "var(--xp-panel-bg, #fff)",
                              color: "var(--xp-text-normal, #000)",
                              font: "inherit",
                            }}
                          />
                        ) : (
                          <>
                            <div style={{ fontWeight: "bold", fontSize: 12, flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis" }}>
                              {t.name}
                            </div>
                            {t.source === "user" && (
                              <div
                                role="button"
                                tabIndex={0}
                                title="Rename"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setRenameDraft(t.name);
                                  setRenamingId(t.id);
                                }}
                                style={{
                                  fontSize: 11,
                                  cursor: "pointer",
                                  padding: "0 4px",
                                  color: "var(--xp-text-muted, #666)",
                                }}
                              >
                                ✎
                              </div>
                            )}
                          </>
                        )}
                      </div>
                      <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>
                        {t.author} · {t.source}
                      </div>
                      <div style={{ fontSize: 10, color: "var(--xp-text-secondary, #555)", lineHeight: 1.35, minHeight: 26 }}>
                        {t.description}
                      </div>
                      <div style={{ display: "flex", gap: 2, marginTop: 2 }}>
                        {swatch.map((c, i) => (
                          <div
                            key={i}
                            style={{
                              width: 28,
                              height: 18,
                              background: c,
                              border: "1px solid var(--xp-border, #888)",
                            }}
                          />
                        ))}
                      </div>
                      <div style={{ marginTop: 8, display: "flex", gap: 12, alignItems: "center" }}>
                        <div
                          role="button"
                          tabIndex={0}
                          onClick={(e) => { e.stopPropagation(); void startCustomizing(t); }}
                          onKeyDown={(e) => { if (e.key === "Enter") { e.stopPropagation(); void startCustomizing(t); } }}
                          title={t.source === "user" ? "Edit this theme" : "Make a customizable copy of this theme"}
                          style={{
                            fontSize: 10,
                            color: "var(--xp-blue, #0058E6)",
                            textDecoration: "underline",
                            cursor: "pointer",
                          }}
                        >
                          {t.source === "user" ? "Edit…" : "Customize…"}
                        </div>
                        {t.source === "user" && (
                          <div
                            role="button"
                            tabIndex={0}
                            onClick={(e) => { e.stopPropagation(); void deleteTheme(t); }}
                            onKeyDown={(e) => { if (e.key === "Enter") { e.stopPropagation(); void deleteTheme(t); } }}
                            title="Delete this theme"
                            style={{
                              fontSize: 10,
                              color: "#a00",
                              textDecoration: "underline",
                              cursor: "pointer",
                            }}
                          >
                            Delete
                          </div>
                        )}
                      </div>
                    </button>
                  );
                })}
              </div>

              {/* Footer actions */}
              <div
                style={{
                  marginTop: "auto",
                  paddingTop: 10,
                  borderTop: "1px solid var(--xp-border, #c8c4b4)",
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  flexWrap: "wrap",
                }}
              >
                <button
                  style={chromeButton}
                  onClick={() =>
                    api.openThemesFolder().catch((e) => setError(String(e)))
                  }
                >
                  Open themes folder
                </button>
                <button style={chromeButton} onClick={refresh} title="Re-scan ~/.farder/themes/">
                  Refresh
                </button>
                <span
                  style={{
                    fontSize: 10,
                    color: "var(--xp-text-muted, #666)",
                    flex: 1,
                    minWidth: 220,
                    lineHeight: 1.4,
                  }}
                >
                  Themes can load external resources. Only use themes from sources you trust.
                </span>
              </div>
            </>
          )}
            </>
          )}
        </div>
      </div>
      {customizing && (
        <CustomizeModal
          themeId={customizing.themeId}
          initialName={customizing.name}
          onClose={() => { setCustomizing(null); refresh(); }}
          onSaved={() => { refresh(); }}
        />
      )}
    </div>
  );
}
