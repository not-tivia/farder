import type { ReactNode } from "react";

interface Props {
  label?: string;
  children: ReactNode;
}

/** A labelled settings section: uppercase label + spaced content block. */
export default function SettingsSection({ label, children }: Props) {
  return (
    <div className="settings-section">
      {label && <div className="settings-section-label">{label}</div>}
      {children}
    </div>
  );
}
