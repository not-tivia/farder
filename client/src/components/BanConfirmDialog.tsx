import { useState, type CSSProperties } from "react";

interface Props {
  targetName: string;
  onCancel: () => void;
  onConfirm: (reason: string) => void;
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
  width: 380,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

export default function BanConfirmDialog({ targetName, onCancel, onConfirm }: Props) {
  const [reason, setReason] = useState("");

  return (
    <div style={overlay} onClick={onCancel}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ marginTop: 0 }}>Ban {targetName}?</h3>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          They won't be able to rejoin with this identity.
        </p>
        <label style={{ fontSize: 11, display: "block", marginTop: 8, marginBottom: 4 }}>
          Reason (optional)
        </label>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          maxLength={200}
          rows={3}
          style={{ width: "100%", font: "inherit", boxSizing: "border-box" }}
        />
        <div style={{ fontSize: 9, color: "var(--xp-text-muted, #888)", textAlign: "right" }}>
          {reason.length}/200
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, marginTop: 12 }}>
          <button onClick={onCancel} style={{ font: "inherit", padding: "4px 12px" }}>
            Cancel
          </button>
          <button
            onClick={() => onConfirm(reason.trim())}
            style={{
              font: "inherit",
              padding: "4px 12px",
              background: "#a00",
              color: "#fff",
              border: "1px solid #800",
            }}
          >
            Ban
          </button>
        </div>
      </div>
    </div>
  );
}
