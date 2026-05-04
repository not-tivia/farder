import { useEffect, useRef, useState, type CSSProperties } from "react";

interface Props {
  /** Initial color (any CSS color string). May be undefined for "no override". */
  value: string | undefined;
  /** Swatches extracted from the active theme to show as quick-picks. */
  themeSwatches: string[];
  /** Called as the user picks; live-preview-friendly. */
  onChange: (color: string) => void;
  /** Called when the user clears the override. */
  onClear: () => void;
  /** Anchor: position the popover near this element. */
  anchorRect: DOMRect;
  onClose: () => void;
}

const swatchStyle: CSSProperties = {
  width: 22,
  height: 22,
  border: "1px solid var(--xp-border, #888)",
  cursor: "pointer",
  padding: 0,
  background: "transparent",
};

export default function ColorPickerPopover({
  value,
  themeSwatches,
  onChange,
  onClear,
  anchorRect,
  onClose,
}: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pickerVal, setPickerVal] = useState<string>(value ?? "#000000");

  // Close on outside click.
  useEffect(() => {
    function handle(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", handle);
    return () => document.removeEventListener("mousedown", handle);
  }, [onClose]);

  return (
    <div
      ref={ref}
      style={{
        position: "fixed",
        top: anchorRect.bottom + 4,
        left: anchorRect.left,
        background: "var(--xp-panel-bg, #fff)",
        color: "var(--xp-text-normal, #000)",
        border: "1px solid var(--xp-border, #888)",
        borderRadius: 4,
        padding: 8,
        boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
        zIndex: 2000,
        display: "flex",
        flexDirection: "column",
        gap: 8,
        minWidth: 180,
      }}
    >
      <div style={{ fontSize: 10, color: "var(--xp-text-muted, #666)" }}>From this theme</div>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
        {themeSwatches.length === 0 && (
          <span style={{ fontSize: 10, color: "var(--xp-text-muted, #888)" }}>(none extracted)</span>
        )}
        {themeSwatches.map((c, i) => (
          <button
            key={i}
            title={c}
            onClick={() => {
              onChange(c);
              setPickerVal(c.startsWith("#") ? c : "#000000");
            }}
            style={{ ...swatchStyle, background: c }}
          />
        ))}
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <span style={{ fontSize: 10 }}>Custom:</span>
        <input
          type="color"
          value={pickerVal}
          onChange={(e) => {
            setPickerVal(e.target.value);
            onChange(e.target.value);
          }}
          style={{ width: 36, height: 24, padding: 0, border: "1px solid var(--xp-border, #888)" }}
        />
        <button
          onClick={onClear}
          title="Clear (use base theme value)"
          style={{
            marginLeft: "auto",
            font: "inherit",
            background: "transparent",
            border: "1px solid var(--xp-border, #888)",
            padding: "2px 8px",
            cursor: "pointer",
            color: "var(--xp-text-normal, #000)",
          }}
        >
          Clear
        </button>
      </div>
    </div>
  );
}
