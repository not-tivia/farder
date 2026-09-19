import { useEffect, useState } from "react";
import AppearanceSettings from "../AppearanceSettings";
import GifSearchSettings from "../GifSearchSettings";
import { TranslationSettingsTab } from "../TranslationSettingsTab";
import VoiceSettings from "../VoiceSettings";
import PrivacyDataSettings from "../PrivacyDataSettings";
import NotificationSettings from "../NotificationSettings";
import AlertSubscriptions from "./AlertSubscriptions";
import MyReminders from "./MyReminders";
import HostedServers from "./HostedServers";

interface Props {
  onClose: () => void;
}

type SectionId = "appearance" | "gif" | "translation" | "voice" | "notifications" | "privacy" | "hosted" | "alerts" | "reminders";

const SECTIONS: { id: SectionId; label: string }[] = [
  { id: "appearance", label: "Appearance" },
  { id: "gif", label: "GIF Search" },
  { id: "translation", label: "Translation" },
  { id: "voice", label: "Voice" },
  { id: "notifications", label: "Notifications" },
  { id: "privacy", label: "Privacy & Data" },
  { id: "hosted", label: "Hosted Servers" },
  { id: "alerts", label: "Alerts" },
  { id: "reminders", label: "Reminders" },
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
          <span>Your Settings &mdash; this device and identity</span>
          <button className="modal-close" onClick={onClose} title="Close">
            &#10005;
          </button>
        </div>
        <div className="settings-layout">
          <nav className="settings-sidebar">
            <div className="settings-nav-group-label">You</div>
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
            {active === "notifications" && <NotificationSettings />}
            {active === "privacy" && <PrivacyDataSettings />}
            {active === "hosted" && <HostedServers />}
            {active === "alerts" && <AlertSubscriptions />}
            {active === "reminders" && <MyReminders />}
          </section>
        </div>
      </div>
    </div>
  );
}
