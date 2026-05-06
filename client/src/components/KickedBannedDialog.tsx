import { type CSSProperties } from "react";

interface Props {
  kind: "kick" | "ban";
  serverName: string;
  reason: string | null;
  onClose: () => void;
}

const overlay: CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,0.55)",
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
  width: 380,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  textAlign: "center",
};

export default function KickedBannedDialog({ kind, serverName, reason, onClose }: Props) {
  const verb = kind === "kick" ? "kicked from" : "banned from";
  return (
    <div style={overlay}>
      <div style={card}>
        <h3 style={{ marginTop: 0 }}>You were {verb} {serverName}</h3>
        {reason && (
          <p style={{ fontSize: 12, color: "var(--xp-text-muted, #666)" }}>
            Reason: {reason}
          </p>
        )}
        <button
          onClick={onClose}
          style={{
            font: "inherit",
            padding: "6px 20px",
            marginTop: 12,
            background: "var(--xp-blue, #0058E6)",
            color: "#fff",
            border: "1px solid var(--xp-blue-dark, #003C74)",
          }}
        >
          OK
        </button>
      </div>
    </div>
  );
}
