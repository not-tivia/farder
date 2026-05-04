import { type CSSProperties } from "react";

interface Props {
  onDismiss: () => void;
}

const overlayStyle: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.5)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 3000,
};

const cardStyle: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  borderRadius: 6,
  padding: 24,
  maxWidth: 480,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function CustomizerIntro({ onDismiss }: Props) {
  return (
    <div style={overlayStyle} onClick={onDismiss}>
      <div style={cardStyle} onClick={(e) => e.stopPropagation()}>
        <h2 style={{ marginTop: 0 }}>Welcome to the Customizer</h2>
        <p>
          Pick a region from the list, change its background color, drop in an image, or change the text color.
          Hit <strong>Save</strong> when you're done. Use <strong>Undo</strong> if you change your mind.
        </p>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          Your edits go into a new theme — built-in themes are never modified.
        </p>
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 16 }}>
          <button
            onClick={onDismiss}
            style={{
              font: "inherit",
              padding: "4px 14px",
              background: "var(--xp-panel-bg, #f0ece0)",
              color: "var(--xp-text-normal, #000)",
              border: "1px solid var(--xp-border, #888)",
              borderRadius: 4,
              cursor: "pointer",
            }}
          >
            Got it
          </button>
        </div>
      </div>
    </div>
  );
}
