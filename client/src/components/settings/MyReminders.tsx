import { useEffect, useState } from "react";
import { listMyReminders, cancelReminder } from "../../lib/tauri-bridge";
import { useActiveServerId } from "../../context/ServerContext";
import type { ReminderInfo } from "../../lib/types";
import SettingsSection from "./SettingsSection";

export default function MyReminders() {
  const serverId = useActiveServerId();
  const [reminders, setReminders] = useState<ReminderInfo[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!serverId) return;
    listMyReminders(serverId)
      .then(setReminders)
      .catch((e) => setError(String(e)));
  }, [serverId]);

  async function handleCancel(id: number) {
    if (!serverId) return;
    setError(null);
    try {
      await cancelReminder(serverId, id);
      setReminders((prev) => prev.filter((r) => r.id !== id));
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">Reminders</h2>

      {error && (
        <div className="error-text" style={{ marginBottom: 8 }}>{error}</div>
      )}

      <SettingsSection label="Upcoming reminders">
        {!serverId && (
          <div style={{ color: "var(--xp-text-muted)", fontSize: 13 }}>
            Connect to a server to manage reminders.
          </div>
        )}
        {serverId && reminders.length === 0 && (
          <div style={{ color: "var(--xp-text-muted)", fontSize: 13 }}>
            You have no upcoming reminders.
          </div>
        )}
        {reminders.map((r) => (
          <div key={r.id} className="organizer-row">
            <span className="organizer-name">
              {r.text} · {new Date(r.due_at * 1000).toLocaleString()}
            </span>
            <div className="organizer-actions">
              <button
                className="organizer-btn organizer-delete"
                title="Cancel reminder"
                onClick={() => void handleCancel(r.id)}
              >
                Cancel
              </button>
            </div>
          </div>
        ))}
      </SettingsSection>
    </div>
  );
}
