import type { CSSProperties, ReactNode } from "react";

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

/**
 * Confirmation for an irreversible action.
 *
 * Its job is not "are you sure" — people click through that — but **stating what
 * is actually lost**, which is why `consequence` is a required prop rather than
 * an optional flourish.
 *
 * Built for the encryption actions (sub-5b G4) and always general: retiring a
 * device, resetting a channel, deleting a channel and everything in it. The
 * encryption side of the app used it from the start while the older UI deleted
 * channels, roles and categories from a context menu on a single click, with the
 * error swallowed — so a failed delete looked exactly like a successful one.
 */
export function ConfirmDialog({
  title,
  consequence,
  confirmLabel,
  busy,
  error,
  onCancel,
  onConfirm,
}: {
  title: string;
  /** What this irreversibly costs, in plain language. Required on purpose. */
  consequence: ReactNode;
  confirmLabel: string;
  busy?: boolean;
  error?: string | null;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <div style={overlay} onClick={busy ? undefined : onCancel}>
      <div style={card} onClick={(e) => e.stopPropagation()}>
        <h3 style={{ margin: "0 0 10px", fontSize: 14 }}>{title}</h3>
        <div style={{ fontSize: 12, lineHeight: 1.5, marginBottom: 14 }}>{consequence}</div>
        {error && (
          <div style={{ fontSize: 12, marginBottom: 10, color: "var(--xp-danger, #a80000)" }}>
            {error}
          </div>
        )}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button className="xp-button" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
          <button className="xp-button" onClick={onConfirm} disabled={busy}>
            {busy ? "Working…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Previous name, kept so the encryption call sites read as they always did. */
export const E2eeConfirmDialog = ConfirmDialog;
