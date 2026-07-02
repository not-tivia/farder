import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString, memberDisplayName } from "../lib/types";

interface Props {
  serverId: string;
}

const MAJORS: { id: string; label: string }[] = [
  { id: "bitcoin", label: "BTC" },
  { id: "ethereum", label: "ETH" },
  { id: "solana", label: "SOL" },
  { id: "litecoin", label: "LTC" },
  { id: "ripple", label: "XRP" },
  { id: "dogecoin", label: "DOGE" },
  { id: "cardano", label: "ADA" },
];

export default function BotsTab({ serverId }: Props) {
  const activeServer = useActiveServer();
  const bots = (activeServer?.members ?? []).filter((m) => m.is_bot);

  const [selectedMajor, setSelectedMajor] = useState<string>(MAJORS[0].id);
  const [isCustom, setIsCustom] = useState(false);
  const [customCoinId, setCustomCoinId] = useState("");
  const [customLabel, setCustomLabel] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [interval, setIntervalSecs] = useState<number>(60);
  useEffect(() => {
    api.getBotPollInterval(serverId).then(setIntervalSecs).catch(() => {});
  }, [serverId]);

  const coinId = isCustom ? customCoinId.trim() : selectedMajor;
  const label = isCustom
    ? customLabel.trim() || customCoinId.trim()
    : (MAJORS.find((m) => m.id === selectedMajor)?.label ?? selectedMajor);

  async function handleAdd() {
    if (!coinId) return;
    setError(null);
    try {
      await api.addBot(serverId, coinId, label);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleRemove(botPublicKey: string) {
    setError(null);
    try {
      await api.removeBot(serverId, botPublicKey);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="organizer-create" style={{ marginTop: 8 }}>
      <div className="connect-section-title" style={{ marginBottom: 8 }}>Bots</div>

      {error && <div className="error-text" style={{ marginBottom: 8 }}>{error}</div>}

      <div style={{ display: "flex", gap: 6, alignItems: "flex-end", marginBottom: 10 }}>
        <div>
          <label className="connect-label">Update interval (seconds, min 30)</label>
          <input
            className="connect-input"
            type="number"
            min={30}
            value={interval}
            onChange={(e) => setIntervalSecs(Number(e.target.value))}
          />
        </div>
        <button
          className="organizer-btn"
          onClick={async () => {
            try { await api.setBotPollInterval(serverId, Math.max(30, Math.floor(interval))); }
            catch (e) { console.error("[bots:set-interval]", e); }
          }}
        >Save</button>
      </div>

      {/* Existing bots */}
      {bots.length === 0 && (
        <div style={{ color: "var(--xp-text-muted)", marginBottom: 8, fontSize: 13 }}>
          No bots on this server yet.
        </div>
      )}
      {bots.map((bot) => (
        <div key={publicKeyToString(bot.public_key)} className="organizer-row">
          <span className="organizer-name">{memberDisplayName(bot.display_name)}</span>
          <div className="organizer-actions">
            <button
              className="organizer-btn organizer-delete"
              title="Remove bot"
              onClick={() => handleRemove(publicKeyToString(bot.public_key))}
            >
              x
            </button>
          </div>
        </div>
      ))}

      {/* Add bot */}
      <div style={{ marginTop: 12, borderTop: "1px solid var(--xp-border)", paddingTop: 10 }}>
        <div className="connect-section-title" style={{ marginBottom: 6, fontSize: 12 }}>Add Ticker Bot</div>
        <div style={{ color: "var(--xp-text-muted)", marginBottom: 8, fontSize: 12 }}>
          A new bot shows &ldquo;fetching price&#x2026;&rdquo; until its first price update (up to ~60s). For a Custom coin, use the exact CoinGecko ID (lowercase, e.g. &ldquo;solana&rdquo;) &mdash; an unknown ID will stay blank.
        </div>
        <div style={{ display: "flex", gap: 6, alignItems: "flex-end", flexWrap: "wrap" }}>
          <div>
            <label className="connect-label">Coin</label>
            <select
              className="connect-input"
              value={isCustom ? "custom" : selectedMajor}
              onChange={(e) => {
                if (e.target.value === "custom") {
                  setIsCustom(true);
                } else {
                  setIsCustom(false);
                  setSelectedMajor(e.target.value);
                }
              }}
            >
              {MAJORS.map((m) => (
                <option key={m.id} value={m.id}>{m.label}</option>
              ))}
              <option value="custom">Custom…</option>
            </select>
          </div>
          {isCustom && (
            <>
              <div>
                <label className="connect-label">CoinGecko ID</label>
                <input
                  className="connect-input"
                  value={customCoinId}
                  onChange={(e) => setCustomCoinId(e.target.value)}
                  placeholder="e.g. polkadot"
                />
              </div>
              <div>
                <label className="connect-label">Label</label>
                <input
                  className="connect-input"
                  value={customLabel}
                  onChange={(e) => setCustomLabel(e.target.value)}
                  placeholder="e.g. DOT"
                />
              </div>
            </>
          )}
          <button
            className="xp-button"
            onClick={handleAdd}
            disabled={!coinId}
          >
            Add
          </button>
        </div>
      </div>
    </div>
  );
}
