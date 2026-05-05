import { type CSSProperties } from "react";

interface Props {
  onDismiss: () => void;
}

const overlay: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.5)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 3000,
};

const card: CSSProperties = {
  background: "var(--xp-window-bg, #ECE9D8)",
  color: "var(--xp-text-normal, #000)",
  border: "2px solid var(--xp-blue-dark, #003C74)",
  padding: 24,
  maxWidth: 480,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function BookIntro({ onDismiss }: Props) {
  return (
    <div style={overlay} onClick={onDismiss}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h2 style={{ marginTop: 0 }}>Welcome to your Reaction Book</h2>
        <p>
          Upload images here, then react with them on any server. Right-click any image in chat to save it directly to your book.
        </p>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          Items are stored locally on your machine and uploaded to a server only when you actually use them there.
        </p>
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 16 }}>
          <button onClick={onDismiss} style={{ font: "inherit", padding: "4px 14px" }}>
            Got it
          </button>
        </div>
      </div>
    </div>
  );
}
