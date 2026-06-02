interface Props {
  selected: boolean;
  label: string;
  description?: string;
  onSelect: () => void;
}

/** Discord-style radio row: filled radio + bold label + description line. */
export default function RadioOption({ selected, label, description, onSelect }: Props) {
  return (
    <button
      type="button"
      className={`settings-option${selected ? " selected" : ""}`}
      aria-pressed={selected}
      onClick={onSelect}
    >
      <span className="settings-option-radio" />
      <span>
        <span className="settings-option-label">{label}</span>
        {description && <span className="settings-option-desc">{description}</span>}
      </span>
    </button>
  );
}
