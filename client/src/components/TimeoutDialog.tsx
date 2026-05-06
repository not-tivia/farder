import { useMemo, useState, type CSSProperties } from "react";

interface Props {
  targetName: string;
  onCancel: () => void;
  onConfirm: (untilMs: number, reason: string | null) => void;
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

const PRESETS: Array<{ label: string; ms: number }> = [
  { label: "60 seconds", ms: 60 * 1000 },
  { label: "5 minutes", ms: 5 * 60 * 1000 },
  { label: "10 minutes", ms: 10 * 60 * 1000 },
  { label: "1 hour", ms: 60 * 60 * 1000 },
  { label: "1 day", ms: 24 * 60 * 60 * 1000 },
  { label: "1 week", ms: 7 * 24 * 60 * 60 * 1000 },
];

const MAX_DURATION_MS = 28 * 24 * 60 * 60 * 1000;

export default function TimeoutDialog({ targetName, onCancel, onConfirm }: Props) {
  const [presetIdx, setPresetIdx] = useState<number>(1); // default 5 minutes
  const [useCustom, setUseCustom] = useState(false);
  const [customAmount, setCustomAmount] = useState<number>(30);
  const [customUnit, setCustomUnit] = useState<"minutes" | "hours" | "days">("minutes");
  const [reason, setReason] = useState("");

  const durationMs = useMemo(() => {
    if (!useCustom) return PRESETS[presetIdx].ms;
    const mult = customUnit === "minutes" ? 60_000 : customUnit === "hours" ? 3_600_000 : 86_400_000;
    return Math.max(1, customAmount) * mult;
  }, [useCustom, presetIdx, customAmount, customUnit]);

  const clampedMs = Math.min(durationMs, MAX_DURATION_MS);
  const untilMs = Date.now() + clampedMs;
  const untilDate = new Date(untilMs);

  return (
    <div style={overlay} onClick={onCancel}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ marginTop: 0 }}>Timeout {targetName}?</h3>
        <p style={{ fontSize: 11, color: "var(--xp-text-muted, #666)" }}>
          They won't be able to send messages, react, or join voice for the chosen duration.
        </p>

        {!useCustom && (
          <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 8 }}>
            {PRESETS.map((p, i) => (
              <label key={i} style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11 }}>
                <input
                  type="radio"
                  name="timeout-preset"
                  checked={presetIdx === i}
                  onChange={() => setPresetIdx(i)}
                />
                {p.label}
              </label>
            ))}
          </div>
        )}

        {useCustom && (
          <div style={{ display: "flex", gap: 6, alignItems: "center", marginTop: 8 }}>
            <input
              type="number"
              min={1}
              value={customAmount}
              onChange={(e) => setCustomAmount(Math.max(1, parseInt(e.target.value || "1", 10)))}
              style={{ width: 80, font: "inherit" }}
            />
            <select
              value={customUnit}
              onChange={(e) => setCustomUnit(e.target.value as "minutes" | "hours" | "days")}
              style={{ font: "inherit" }}
            >
              <option value="minutes">minutes</option>
              <option value="hours">hours</option>
              <option value="days">days</option>
            </select>
          </div>
        )}

        <label style={{ fontSize: 11, display: "block", marginTop: 8 }}>
          <input
            type="checkbox"
            checked={useCustom}
            onChange={(e) => setUseCustom(e.target.checked)}
            style={{ marginRight: 6 }}
          />
          Custom duration (max 28 days)
        </label>

        <label style={{ fontSize: 11, display: "block", marginTop: 12, marginBottom: 4 }}>
          Reason (optional)
        </label>
        <textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          maxLength={200}
          rows={2}
          style={{ width: "100%", font: "inherit", boxSizing: "border-box" }}
        />
        <div style={{ fontSize: 9, color: "var(--xp-text-muted, #888)", textAlign: "right" }}>
          {reason.length}/200
        </div>

        <p style={{ fontSize: 10, color: "var(--xp-text-muted, #666)", marginTop: 4 }}>
          Until {untilDate.toLocaleString()}
        </p>

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 6, marginTop: 12 }}>
          <button onClick={onCancel} style={{ font: "inherit", padding: "4px 12px" }}>
            Cancel
          </button>
          <button
            onClick={() => onConfirm(untilMs, reason.trim() || null)}
            style={{
              font: "inherit",
              padding: "4px 12px",
              background: "#a60",
              color: "#fff",
              border: "1px solid #840",
            }}
          >
            Time out
          </button>
        </div>
      </div>
    </div>
  );
}
