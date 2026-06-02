interface Props {
  label: string;
  keyLabel: string;
  capturing?: boolean;
  onRebind: () => void;
}

/** A row showing the current key as a chip with a Rebind button. */
export default function KeybindRow({ label, keyLabel, capturing, onRebind }: Props) {
  return (
    <div className="settings-keybind">
      <span className="settings-keybind-label">{label}</span>
      <kbd className="settings-kbd">{keyLabel}</kbd>
      <button type="button" className="settings-btn" onClick={onRebind}>
        {capturing ? "Press a key..." : "Rebind"}
      </button>
    </div>
  );
}
