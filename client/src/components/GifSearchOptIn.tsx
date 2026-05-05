import { type CSSProperties } from "react";

interface Props {
  onCancel: () => void;
  onEnable: () => void;
}

const overlay: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.4)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2400,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 20,
  width: 420,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function GifSearchOptIn({ onCancel, onEnable }: Props) {
  return (
    <div style={overlay} onClick={onCancel}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ marginTop: 0 }}>Enable GIF search?</h3>
        <p style={{ fontSize: 11, lineHeight: 1.5 }}>
          GIF search uses Tenor (owned by Google). When enabled, Tenor will see your search terms and your IP address. NSFW content is filtered out by default.
        </p>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          You can disable this anytime in Settings → GIF Search.
        </p>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, marginTop: 16 }}>
          <button onClick={onCancel} style={{ font: "inherit", padding: "4px 12px" }}>
            Cancel
          </button>
          <button
            onClick={onEnable}
            style={{
              font: "inherit",
              padding: "4px 12px",
              background: "var(--xp-blue, #0058E6)",
              color: "#fff",
              border: "1px solid var(--xp-blue-dark, #003C74)",
            }}
          >
            Enable
          </button>
        </div>
      </div>
    </div>
  );
}
