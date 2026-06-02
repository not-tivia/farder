import { useEffect, useState } from "react";
import AppearanceSettings from "../AppearanceSettings";
import GifSearchSettings from "../GifSearchSettings";
import { TranslationSettingsTab } from "../TranslationSettingsTab";
import VoiceSettings from "../VoiceSettings";

interface Props {
  onClose: () => void;
}

type SectionId = "appearance" | "gif" | "translation" | "voice";

const SECTIONS: { id: SectionId; label: string }[] = [
  { id: "appearance", label: "Appearance" },
  { id: "gif", label: "GIF Search" },
  { id: "translation", label: "Translation" },
  { id: "voice", label: "Voice" },
];

export default function SettingsModal({ onClose }: Props) {
  const [active, setActive] = useState<SectionId>("appearance");

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Don't close if another handler already consumed the key (e.g. the
      // Voice panel capturing a push-to-talk rebind).
      if (e.defaultPrevented) return;
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog settings-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Settings</span>
          <button className="modal-close" onClick={onClose} title="Close">
            &#10005;
          </button>
        </div>
        <div className="settings-layout">
          <nav className="settings-sidebar">
            <div className="settings-nav-group-label">Settings</div>
            {SECTIONS.map((s) => (
              <button
                key={s.id}
                className={`settings-nav-item${active === s.id ? " active" : ""}`}
                onClick={() => setActive(s.id)}
              >
                {s.label}
              </button>
            ))}
          </nav>
          <section className="settings-content">
            {active === "appearance" && <AppearanceSettings />}
            {active === "gif" && <GifSearchSettings />}
            {active === "translation" && <TranslationSettingsTab />}
            {active === "voice" && <VoiceSettings />}
          </section>
        </div>
      </div>
    </div>
  );
}
