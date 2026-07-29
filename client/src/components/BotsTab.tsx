import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import { useActiveServer } from "../context/ServerContext";
import { publicKeyToString, memberDisplayName, type BotAlertInfo, type CommandInfo } from "../lib/types";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";

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

  // Custom monitor form state
  const [cmName, setCmName] = useState("");
  const [cmUrl, setCmUrl] = useState("");
  const [cmPath, setCmPath] = useState("");
  const [cmUnit, setCmUnit] = useState("");
  const [cmError, setCmError] = useState<string | null>(null);

  // Commands state
  const [ownPk, setOwnPk] = useState<string | null>(null);
  const [commands, setCommands] = useState<CommandInfo[]>([]);
  const [cmdName, setCmdName] = useState("");
  const [cmdTrigger, setCmdTrigger] = useState("");
  const [cmdDescription, setCmdDescription] = useState("");
  const [cmdKind, setCmdKind] = useState<"text" | "api" | "poll" | "giveaway" | "event" | "reminder">("text");
  const [cmdBody, setCmdBody] = useState("");
  const [cmdUrl, setCmdUrl] = useState("");
  const [cmdPath, setCmdPath] = useState("");
  const [cmdRespTemplate, setCmdRespTemplate] = useState("");
  const [cmdUnit, setCmdUnit] = useState("");
  const [cmdError, setCmdError] = useState<string | null>(null);

  // Per-bot alerts state
  const [alertsExpanded, setAlertsExpanded] = useState<Record<string, boolean>>({});
  const [botAlerts, setBotAlerts] = useState<Record<string, BotAlertInfo[]>>({});
  const [alertMetric, setAlertMetric] = useState<Record<string, string>>({});
  const [alertComparator, setAlertComparator] = useState<Record<string, string>>({});
  const [alertThreshold, setAlertThreshold] = useState<Record<string, string>>({});
  const [alertError, setAlertError] = useState<Record<string, string | null>>({});

  useEffect(() => {
    api.getBotPollInterval(serverId).then(setIntervalSecs).catch(() => {});
  }, [serverId]);

  useEffect(() => {
    api.getPublicKey().then(setOwnPk).catch(() => {});
  }, []);

  useEffect(() => {
    api.listCommands(serverId).then(setCommands).catch(() => {});
  }, [serverId]);

  async function loadCommands() {
    try {
      const list = await api.listCommands(serverId);
      setCommands(list);
    } catch {
      // silent
    }
  }

  async function loadAlerts(botPk: string) {
    try {
      const alerts = await api.listBotAlerts(serverId, botPk);
      setBotAlerts((prev) => ({ ...prev, [botPk]: alerts }));
    } catch (e) {
      setAlertError((prev) => ({ ...prev, [botPk]: String(e) }));
    }
  }

  function toggleAlertsExpanded(botPk: string) {
    const next = !alertsExpanded[botPk];
    setAlertsExpanded((prev) => ({ ...prev, [botPk]: next }));
    if (next && !botAlerts[botPk]) {
      void loadAlerts(botPk);
    }
  }

  async function handleRemoveAlert(botPk: string, alertId: number) {
    setAlertError((prev) => ({ ...prev, [botPk]: null }));
    try {
      await api.removeBotAlert(serverId, alertId);
      await loadAlerts(botPk);
    } catch (e) {
      setAlertError((prev) => ({ ...prev, [botPk]: String(e) }));
    }
  }

  async function handleAddAlert(botPk: string) {
    const metric = alertMetric[botPk] ?? "price_usd";
    const comparator = alertComparator[botPk] ?? "above";
    const threshold = parseFloat(alertThreshold[botPk] ?? "");
    if (isNaN(threshold)) {
      setAlertError((prev) => ({ ...prev, [botPk]: "Enter a valid number for threshold" }));
      return;
    }
    setAlertError((prev) => ({ ...prev, [botPk]: null }));
    try {
      await api.addBotAlert(serverId, botPk, metric, comparator, threshold);
      setAlertThreshold((prev) => ({ ...prev, [botPk]: "" }));
      await loadAlerts(botPk);
    } catch (e) {
      setAlertError((prev) => ({ ...prev, [botPk]: String(e) }));
    }
  }

  const members = activeServer?.members ?? [];
  const roles = activeServer?.roles ?? [];
  const bits = ownPk
    ? getActorPermissions(members, roles, ownPk, activeServer?.ownerPublicKey ?? null).bits
    : 0n;
  const canManageServer = hasPermission(bits, PERMISSIONS.MANAGE_SERVER);

  async function handleAddCommand() {
    setCmdError(null);
    const isWidgetKind =
      cmdKind === "poll" || cmdKind === "giveaway" || cmdKind === "event" || cmdKind === "reminder";
    try {
      await api.addCommand(
        serverId,
        cmdName.trim(),
        cmdTrigger.trim().toLowerCase(),
        cmdDescription.trim(),
        cmdKind,
        cmdKind === "text" ? cmdBody.trim() : null,
        cmdKind === "api" ? cmdUrl.trim() : null,
        cmdKind === "api" ? cmdPath.trim() : null,
        isWidgetKind ? null : cmdRespTemplate.trim() || null,
        isWidgetKind ? null : cmdUnit.trim() || null,
      );
      setCmdName("");
      setCmdTrigger("");
      setCmdDescription("");
      setCmdBody("");
      setCmdUrl("");
      setCmdPath("");
      setCmdRespTemplate("");
      setCmdUnit("");
      await loadCommands();
    } catch (e) {
      setCmdError(String(e));
    }
  }

  async function handleDeleteCommand(id: number) {
    setCmdError(null);
    try {
      await api.deleteCommand(serverId, id);
      await loadCommands();
    } catch (e) {
      setCmdError(String(e));
    }
  }

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

  async function handleAddCustomBot() {
    setCmError(null);
    try {
      await api.addCustomBot(serverId, cmName.trim(), cmUrl.trim(), cmPath.trim(), cmUnit.trim() || null);
      setCmName("");
      setCmUrl("");
      setCmPath("");
      setCmUnit("");
    } catch (e) {
      setCmError(String(e));
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
      {bots.map((bot) => {
        const botPk = publicKeyToString(bot.public_key);
        const expanded = alertsExpanded[botPk] ?? false;
        const alerts = botAlerts[botPk] ?? [];
        const botAlertErr = alertError[botPk] ?? null;
        return (
          <div key={botPk} className="organizer-row" style={{ flexDirection: "column", alignItems: "stretch" }}>
            <div style={{ display: "flex", alignItems: "center" }}>
              <span className="organizer-name">{memberDisplayName(bot.display_name)}</span>
              <div className="organizer-actions">
                <button
                  className="organizer-btn"
                  title={expanded ? "Hide alerts" : "Alerts"}
                  onClick={() => toggleAlertsExpanded(botPk)}
                  style={{ marginRight: 4 }}
                >
                  {expanded ? "▲ Alerts" : "▼ Alerts"}
                </button>
                <button
                  className="organizer-btn organizer-delete"
                  title="Remove bot"
                  onClick={() => handleRemove(botPk)}
                >
                  x
                </button>
              </div>
            </div>

            {expanded && (
              <div style={{ paddingLeft: 8, paddingTop: 6, paddingBottom: 4 }}>
                {botAlertErr && (
                  <div className="error-text" style={{ marginBottom: 4 }}>{botAlertErr}</div>
                )}
                {alerts.length === 0 && (
                  <div style={{ color: "var(--xp-text-muted)", fontSize: 12, marginBottom: 6 }}>
                    No alerts set for this bot.
                  </div>
                )}
                {alerts.map((alert) => (
                  <div key={alert.id} className="organizer-row" style={{ marginBottom: 4 }}>
                    <span className="organizer-name" style={{ fontSize: 12 }}>
                      {alert.metric} {alert.comparator} {alert.threshold}
                    </span>
                    <div className="organizer-actions">
                      <button
                        className="organizer-btn organizer-delete"
                        title="Remove alert"
                        onClick={() => void handleRemoveAlert(botPk, alert.id)}
                      >
                        x
                      </button>
                    </div>
                  </div>
                ))}

                {/* Add alert row */}
                <div style={{ display: "flex", gap: 4, alignItems: "flex-end", flexWrap: "wrap", marginTop: 4 }}>
                  <div>
                    <label className="connect-label" style={{ fontSize: 11 }}>Metric</label>
                    <select
                      className="connect-input"
                      value={alertMetric[botPk] ?? "price_usd"}
                      onChange={(e) => setAlertMetric((prev) => ({ ...prev, [botPk]: e.target.value }))}
                    >
                      <option value="price_usd">Price</option>
                      <option value="change_24h">24h change</option>
                      <option value="value">Value (custom bots)</option>
                    </select>
                  </div>
                  <div>
                    <label className="connect-label" style={{ fontSize: 11 }}>Condition</label>
                    <select
                      className="connect-input"
                      value={alertComparator[botPk] ?? "above"}
                      onChange={(e) => setAlertComparator((prev) => ({ ...prev, [botPk]: e.target.value }))}
                    >
                      <option value="above">above</option>
                      <option value="below">below</option>
                    </select>
                  </div>
                  <div>
                    <label className="connect-label" style={{ fontSize: 11 }}>Value</label>
                    <input
                      className="connect-input"
                      type="number"
                      placeholder="e.g. 70000"
                      value={alertThreshold[botPk] ?? ""}
                      onChange={(e) => setAlertThreshold((prev) => ({ ...prev, [botPk]: e.target.value }))}
                      style={{ width: 90 }}
                    />
                  </div>
                  <button
                    className="organizer-btn"
                    onClick={() => void handleAddAlert(botPk)}
                  >
                    Add
                  </button>
                </div>
              </div>
            )}
          </div>
        );
      })}

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

      {/* Add Custom Monitor */}
      <div style={{ marginTop: 12, borderTop: "1px solid var(--xp-border)", paddingTop: 10 }}>
        <div className="connect-section-title" style={{ marginBottom: 6, fontSize: 12 }}>Add Custom Monitor</div>
        <div style={{ color: "var(--xp-text-muted)", marginBottom: 8, fontSize: 12 }}>
          Polls any JSON API and broadcasts the extracted value as the bot&apos;s presence. The value path is a dot-separated key chain (e.g. <code>data.players</code>).
        </div>
        {cmError && <div className="error-text" style={{ marginBottom: 6 }}>{cmError}</div>}
        <div style={{ display: "flex", gap: 6, alignItems: "flex-end", flexWrap: "wrap" }}>
          <div>
            <label className="connect-label">Name</label>
            <input
              className="connect-input"
              value={cmName}
              onChange={(e) => setCmName(e.target.value)}
              placeholder="e.g. Players Online"
            />
          </div>
          <div>
            <label className="connect-label">API URL</label>
            <input
              className="connect-input"
              value={cmUrl}
              onChange={(e) => setCmUrl(e.target.value)}
              placeholder="https://api.example.com/stats"
            />
          </div>
          <div>
            <label className="connect-label">Value path</label>
            <input
              className="connect-input"
              value={cmPath}
              onChange={(e) => setCmPath(e.target.value)}
              placeholder="data.players"
            />
          </div>
          <div>
            <label className="connect-label">Unit (optional)</label>
            <input
              className="connect-input"
              value={cmUnit}
              onChange={(e) => setCmUnit(e.target.value)}
              placeholder="e.g. players"
              style={{ width: 90 }}
            />
          </div>
          <button
            className="xp-button"
            onClick={handleAddCustomBot}
            disabled={!cmName.trim() || !cmUrl.trim() || !cmPath.trim()}
          >
            Add
          </button>
        </div>
      </div>
      {/* Slash Commands */}
      <div style={{ marginTop: 12, borderTop: "1px solid var(--xp-border)", paddingTop: 10 }}>
        <div className="connect-section-title" style={{ marginBottom: 6, fontSize: 12 }}>Slash Commands</div>
        <div style={{ color: "var(--xp-text-muted)", marginBottom: 8, fontSize: 12 }}>
          Commands are invoked with <code>/trigger</code> (or <code>/trigger arg</code>) in any channel.
        </div>

        {cmdError && <div className="error-text" style={{ marginBottom: 6 }}>{cmdError}</div>}

        {/* Existing commands */}
        {commands.length === 0 && (
          <div style={{ color: "var(--xp-text-muted)", marginBottom: 8, fontSize: 13 }}>
            No commands on this server yet.
          </div>
        )}
        {commands.map((cmd) => (
          <div key={cmd.id} className="organizer-row">
            <span className="organizer-name" style={{ fontSize: 13 }}>
              <code>/{cmd.trigger}</code>
              {cmd.description ? ` — ${cmd.description}` : ""}
            </span>
            {canManageServer && (
              <div className="organizer-actions">
                <button
                  className="organizer-btn organizer-delete"
                  title="Delete command"
                  onClick={() => void handleDeleteCommand(cmd.id)}
                >
                  x
                </button>
              </div>
            )}
          </div>
        ))}

        {/* Add Command form (MANAGE_SERVER gated) */}
        {canManageServer && (
          <>
            <div style={{ display: "flex", gap: 6, alignItems: "flex-end", flexWrap: "wrap", marginTop: 8 }}>
              <div>
                <label className="connect-label">Name</label>
                <input
                  className="connect-input"
                  value={cmdName}
                  onChange={(e) => setCmdName(e.target.value)}
                  placeholder="e.g. Weather"
                />
              </div>
              <div>
                <label className="connect-label">Trigger</label>
                <input
                  className="connect-input"
                  value={cmdTrigger}
                  onChange={(e) => setCmdTrigger(e.target.value)}
                  placeholder="weather"
                  style={{ width: 90 }}
                />
              </div>
              <div>
                <label className="connect-label">Description</label>
                <input
                  className="connect-input"
                  value={cmdDescription}
                  onChange={(e) => setCmdDescription(e.target.value)}
                  placeholder="Gets the weather"
                />
              </div>
              <div>
                <label className="connect-label">Kind</label>
                <select
                  className="connect-input"
                  value={cmdKind}
                  onChange={(e) =>
                    setCmdKind(e.target.value as "text" | "api" | "poll" | "giveaway" | "event" | "reminder")
                  }
                >
                  <option value="text">text</option>
                  <option value="api">api</option>
                  <option value="poll">Poll</option>
                  <option value="giveaway">Giveaway</option>
                  <option value="event">Event</option>
                  <option value="reminder">Reminder</option>
                </select>
              </div>
            </div>

            {cmdKind === "text" && (
              <div style={{ marginTop: 6 }}>
                <label className="connect-label">Response text</label>
                <textarea
                  className="connect-input"
                  value={cmdBody}
                  onChange={(e) => setCmdBody(e.target.value)}
                  placeholder="Hello, {arg}!"
                  rows={3}
                  style={{ width: "100%", boxSizing: "border-box", resize: "vertical" }}
                />
              </div>
            )}

            {cmdKind === "poll" && (
              <div style={{ color: "var(--xp-text-muted)", marginTop: 6, fontSize: 12 }}>
                Members run <code>{"/<trigger> Question | option A | option B [| 30m|2h|1d]"}</code>
              </div>
            )}

            {cmdKind === "giveaway" && (
              <div style={{ color: "var(--xp-text-muted)", marginTop: 6, fontSize: 12 }}>
                Usage: <code>{"/<trigger> <duration> <prize>"}</code> — e.g. <code>/giveaway 24h Steam key</code> (moderators only)
              </div>
            )}

            {cmdKind === "event" && (
              <div style={{ color: "var(--xp-text-muted)", marginTop: 6, fontSize: 12 }}>
                Members run <code>{"/<trigger> Title | 3d [| location] [| description] [| remind 1h]"}</code> — or just pick it from &quot;/&quot; to open the form.
              </div>
            )}

            {cmdKind === "reminder" && (
              <div style={{ color: "var(--xp-text-muted)", marginTop: 6, fontSize: 12 }}>
                Members run <code>{"/<trigger> 90m take the pizza out"}</code> — private, nothing is posted.
              </div>
            )}

            {cmdKind === "api" && (
              <div style={{ display: "flex", gap: 6, alignItems: "flex-end", flexWrap: "wrap", marginTop: 6 }}>
                <div>
                  <label className="connect-label">URL template</label>
                  <input
                    className="connect-input"
                    value={cmdUrl}
                    onChange={(e) => setCmdUrl(e.target.value)}
                    placeholder="https://api.example.com/{arg}"
                    style={{ width: 220 }}
                  />
                </div>
                <div>
                  <label className="connect-label">Value path</label>
                  <input
                    className="connect-input"
                    value={cmdPath}
                    onChange={(e) => setCmdPath(e.target.value)}
                    placeholder="data.value"
                  />
                </div>
                <div>
                  <label className="connect-label">Response template (optional)</label>
                  <input
                    className="connect-input"
                    value={cmdRespTemplate}
                    onChange={(e) => setCmdRespTemplate(e.target.value)}
                    placeholder="{arg}: {value}"
                  />
                </div>
                <div>
                  <label className="connect-label">Unit (optional)</label>
                  <input
                    className="connect-input"
                    value={cmdUnit}
                    onChange={(e) => setCmdUnit(e.target.value)}
                    placeholder="e.g. °F"
                    style={{ width: 80 }}
                  />
                </div>
              </div>
            )}

            <div style={{ marginTop: 6 }}>
              <button
                className="xp-button"
                onClick={handleAddCommand}
                disabled={
                  !cmdName.trim() ||
                  !cmdTrigger.trim() ||
                  !cmdDescription.trim() ||
                  (cmdKind === "text"
                    ? !cmdBody.trim()
                    : cmdKind === "api"
                      ? !cmdUrl.trim() || !cmdPath.trim()
                      : false)
                }
              >
                Add Command
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
