import { useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import ColorPickerPopover from "./ColorPickerPopover";
import type { RegionDefinition, RegionState, ImageFit } from "../lib/customizer/types";

interface Props {
  region: RegionDefinition;
  state: RegionState | undefined;
  themeId: string;
  themeSwatches: string[];
  onChange: (next: RegionState | undefined) => void;
  onError: (msg: string) => void;
}

const swatchBtn: CSSProperties = {
  width: 28,
  height: 22,
  border: "1px solid var(--xp-border, #888)",
  cursor: "pointer",
  padding: 0,
};

const chip: CSSProperties = {
  font: "inherit",
  background: "transparent",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  borderRadius: 3,
  padding: "2px 8px",
  cursor: "pointer",
  whiteSpace: "nowrap",
};

const FIT_OPTIONS: ImageFit[] = ["stretch", "tile", "center", "cover"];

export default function CustomizerRegionRow({
  region,
  state,
  themeId,
  themeSwatches,
  onChange,
  onError,
}: Props) {
  const bgRef = useRef<HTMLButtonElement | null>(null);
  const textRef = useRef<HTMLButtonElement | null>(null);
  const [openPicker, setOpenPicker] = useState<"bg" | "text" | null>(null);

  const bgColor = state?.bgColor;
  const bgImage = state?.bgImage;
  const textColor = state?.textColor;

  function patch(p: Partial<RegionState>): void {
    const next: RegionState = {
      bgColor: p.bgColor !== undefined ? (p.bgColor || undefined) : bgColor,
      bgImage: p.bgImage !== undefined ? (p.bgImage || undefined) : bgImage,
      textColor: p.textColor !== undefined ? (p.textColor || undefined) : textColor,
    };
    if (next.bgColor === undefined && next.bgImage === undefined && next.textColor === undefined) {
      onChange(undefined);
    } else {
      onChange(next);
    }
  }

  async function pickImage(): Promise<void> {
    try {
      const selected = await api.pickFile();
      if (!selected) return;

      // Light extension check (the user could pick anything; we just want to
      // catch obvious mistakes early before paying the round-trip).
      const lower = selected.toLowerCase();
      const validExt = [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"].some((ext) => lower.endsWith(ext));
      if (!validExt) {
        onError("Please pick an image file (.png, .jpg, .jpeg, .webp, .gif, .bmp).");
        return;
      }

      const original = selected.split(/[\\/]/).pop() ?? "image";
      const stamped = `${Date.now()}-${original}`;
      const relPath = await api.addThemeAsset(themeId, selected, stamped);

      patch({ bgImage: { path: relPath, fit: bgImage?.fit ?? "cover" } });
    } catch (e) {
      onError(String(e));
    }
  }

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "200px auto auto auto 1fr auto auto",
        gap: 8,
        alignItems: "center",
        padding: "6px 0",
        borderBottom: "1px solid var(--xp-border, #d8d4c4)",
      }}
    >
      <div style={{ fontWeight: "bold" }}>{region.label}</div>

      {/* Background color swatch */}
      <button
        ref={bgRef}
        onClick={() => setOpenPicker(openPicker === "bg" ? null : "bg")}
        title={bgColor ? `Background: ${bgColor}` : "Set background color"}
        style={{
          ...swatchBtn,
          background: bgColor ?? "repeating-linear-gradient(45deg, #eee, #eee 4px, #ccc 4px, #ccc 8px)",
        }}
      />

      {/* Image picker */}
      {region.hasImage ? (
        <button style={chip} onClick={pickImage} title="Pick a background image">
          {bgImage ? "Change image…" : "Pick image…"}
        </button>
      ) : (
        <span />
      )}

      {/* Fit dropdown — only when an image is set */}
      {region.hasImage && bgImage ? (
        <select
          value={bgImage.fit}
          onChange={(e) => patch({ bgImage: { path: bgImage.path, fit: e.target.value as ImageFit } })}
          style={{
            font: "inherit",
            background: "var(--xp-panel-bg, #fff)",
            color: "var(--xp-text-normal, #000)",
            border: "1px solid var(--xp-border, #888)",
            padding: "2px 4px",
          }}
        >
          {FIT_OPTIONS.map((f) => (
            <option key={f} value={f}>{f}</option>
          ))}
        </select>
      ) : (
        <span />
      )}

      <span /> {/* spacer */}

      {/* Text color */}
      {region.hasText ? (
        <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
          <span style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>text:</span>
          <button
            ref={textRef}
            onClick={() => setOpenPicker(openPicker === "text" ? null : "text")}
            title={textColor ? `Text color: ${textColor}` : "Set text color"}
            style={{
              ...swatchBtn,
              background: textColor ?? "repeating-linear-gradient(45deg, #eee, #eee 4px, #ccc 4px, #ccc 8px)",
            }}
          />
        </div>
      ) : (
        <span />
      )}

      {/* Clear-all-for-this-region button */}
      <button
        style={{ ...chip, padding: "2px 6px" }}
        onClick={() => onChange(undefined)}
        title="Clear all overrides for this region"
      >
        ×
      </button>

      {/* Popovers */}
      {openPicker === "bg" && bgRef.current && (
        <ColorPickerPopover
          value={bgColor}
          themeSwatches={themeSwatches}
          anchorRect={bgRef.current.getBoundingClientRect()}
          onChange={(c) => patch({ bgColor: c, bgImage: undefined })}
          onClear={() => {
            patch({ bgColor: undefined });
            setOpenPicker(null);
          }}
          onClose={() => setOpenPicker(null)}
        />
      )}
      {openPicker === "text" && textRef.current && (
        <ColorPickerPopover
          value={textColor}
          themeSwatches={themeSwatches}
          anchorRect={textRef.current.getBoundingClientRect()}
          onChange={(c) => patch({ textColor: c })}
          onClear={() => {
            patch({ textColor: undefined });
            setOpenPicker(null);
          }}
          onClose={() => setOpenPicker(null)}
        />
      )}
    </div>
  );
}
