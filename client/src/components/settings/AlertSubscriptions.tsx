import { useEffect, useState } from "react";
import { listMySubscriptions, unsubscribeBot } from "../../lib/tauri-bridge";
import { useActiveServer, useActiveServerId } from "../../context/ServerContext";
import { publicKeyToString, memberDisplayName } from "../../lib/types";
import SettingsSection from "./SettingsSection";

export default function AlertSubscriptions() {
  const serverId = useActiveServerId();
  const activeServer = useActiveServer();
  const [subscriptions, setSubscriptions] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!serverId) return;
    listMySubscriptions(serverId)
      .then(setSubscriptions)
      .catch((e) => setError(String(e)));
  }, [serverId]);

  async function handleUnsubscribe(botPk: string) {
    if (!serverId) return;
    setError(null);
    try {
      await unsubscribeBot(serverId, botPk);
      setSubscriptions((prev) => prev.filter((k) => k !== botPk));
    } catch (e) {
      setError(String(e));
    }
  }

  const botMembers = (activeServer?.members ?? []).filter((m) => m.is_bot);

  function botLabel(pk: string): string {
    const match = botMembers.find((m) => publicKeyToString(m.public_key) === pk);
    return match ? memberDisplayName(match.display_name) : pk.slice(0, 18) + "…";
  }

  return (
    <div className="settings-panel">
      <h2 className="settings-panel-title">Alerts</h2>

      {error && (
        <div className="error-text" style={{ marginBottom: 8 }}>{error}</div>
      )}

      <SettingsSection label="Bot Subscriptions">
        {!serverId && (
          <div style={{ color: "var(--xp-text-muted)", fontSize: 13 }}>
            Connect to a server to manage subscriptions.
          </div>
        )}
        {serverId && subscriptions.length === 0 && (
          <div style={{ color: "var(--xp-text-muted)", fontSize: 13 }}>
            You are not subscribed to any bots on this server.
          </div>
        )}
        {subscriptions.map((pk) => (
          <div key={pk} className="organizer-row">
            <span className="organizer-name">{botLabel(pk)}</span>
            <div className="organizer-actions">
              <button
                className="organizer-btn organizer-delete"
                title="Unsubscribe"
                onClick={() => void handleUnsubscribe(pk)}
              >
                Unsubscribe
              </button>
            </div>
          </div>
        ))}
      </SettingsSection>
    </div>
  );
}
