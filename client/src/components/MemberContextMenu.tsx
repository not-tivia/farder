import { useEffect, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import type { MemberInfo, RoleInfo } from "../lib/types";
import { publicKeyToString } from "../lib/types";
import { useActiveServer } from "../context/ServerContext";
import { getActorPermissions, hasPermission, PERMISSIONS } from "../lib/permissions";
import BanConfirmDialog from "./BanConfirmDialog";
import TimeoutDialog from "./TimeoutDialog";
import UserProfilePopup from "./UserProfilePopup";
import { getTranslationSettings, setTranslationSettings } from "../lib/translation/api";
import { SourceLanguagePicker } from "./SourceLanguagePicker";
import { useClickAnchoredPosition } from "../lib/useClickAnchoredPosition";
import { isE2eeChannel } from "../lib/types";
import { E2eeMemberDevices } from "./E2eeMemberDevices";

interface Props {
  target: MemberInfo;
  serverId: string;
  position: { x: number; y: number };
  ownPk: string | null;
  onClose: () => void;
}

const menuStyle: CSSProperties = {
  position: "fixed",
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 4,
  minWidth: 180,
  zIndex: 2500,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
};

const itemStyle: CSSProperties = {
  display: "block",
  width: "100%",
  textAlign: "left",
  padding: "4px 10px",
  background: "transparent",
  border: "none",
  cursor: "pointer",
  font: "inherit",
  color: "inherit",
};

const separatorStyle: CSSProperties = {
  height: 1,
  background: "var(--xp-border, #ccc)",
  margin: "4px 0",
};

const submenuStyle: CSSProperties = {
  ...menuStyle,
  position: "absolute",
  top: 0,
  left: "100%",
  marginLeft: 2,
  maxHeight: 280,
  overflowY: "auto",
};

export default function MemberContextMenu({ target, serverId, position, ownPk, onClose }: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const activeServer = useActiveServer();
  const [showDevices, setShowDevices] = useState(false);
  const logServerId = activeServer?.logServerId ?? null;
  const currentChannelId = activeServer?.currentChannelId ?? null;
  const e2eeChannelId = isE2eeChannel(
    currentChannelId != null ? activeServer?.channels.find((c) => c.id === currentChannelId) ?? null : null,
  )
    ? currentChannelId
    : null;
  const [showRoleSubmenu, setShowRoleSubmenu] = useState(false);
  const [showLanguageSubmenu, setShowLanguageSubmenu] = useState(false);
  const [overrideValue, setOverrideValue] = useState<string | null>(null);
  const [translationTarget, setTranslationTarget] = useState<string>("en");
  const [showBanDialog, setShowBanDialog] = useState(false);
  const [showTimeoutDialog, setShowTimeoutDialog] = useState(false);
  const [showProfile, setShowProfile] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isSubscribed, setIsSubscribed] = useState<boolean>(false);
  const targetPk = publicKeyToString(target.public_key);
  const isSelf = ownPk === targetPk;
  const members = activeServer?.members ?? [];
  const roles: RoleInfo[] = activeServer?.roles ?? [];

  const { bits } = ownPk
    ? getActorPermissions(members, roles, ownPk, activeServer?.ownerPublicKey ?? null)
    : { bits: 0n };

  const canManageRoles = hasPermission(bits, PERMISSIONS.MANAGE_ROLES);
  const canKick = hasPermission(bits, PERMISSIONS.KICK_MEMBERS);
  const canBan = hasPermission(bits, PERMISSIONS.BAN_MEMBERS);
  const canTimeout = hasPermission(bits, PERMISSIONS.TIMEOUT_MEMBERS);
  const isTargetTimedOut = !!(target.timeout_until && target.timeout_until > Date.now());

  // When any modal child (Ban/Timeout/Profile) is open, hide the menu and
  // disable its outside-click handler so the modal can be interacted with.
  const dialogOpen = showBanDialog || showTimeoutDialog || showProfile;

  useEffect(() => {
    if (dialogOpen) return;
    function handleMouse(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", handleMouse);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleMouse);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose, dialogOpen]);

  useEffect(() => {
    (async () => {
      try {
        const settings = await getTranslationSettings();
        setTranslationTarget(settings.default_target);
        setOverrideValue(settings.user_language_overrides[targetPk] ?? null);
      } catch {
        // silent: settings fetch failures fall through to defaults
      }
    })();
  }, [targetPk]);

  useEffect(() => {
    if (!target.is_bot) return;
    (async () => {
      try {
        const subs = await api.listMySubscriptions(serverId);
        setIsSubscribed(subs.includes(targetPk));
      } catch {
        // silent: subscription state is best-effort
      }
    })();
  }, [target.is_bot, serverId, targetPk]);

  function viewProfile() {
    setShowProfile(true);
  }

  async function sendMessage() {
    try {
      await api.openDm(serverId, targetPk);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function toggleRole(roleId: number) {
    const has = target.role_ids?.includes(roleId);
    try {
      if (has) await api.removeRole(serverId, targetPk, roleId);
      else await api.assignRole(serverId, targetPk, roleId);
    } catch (e) {
      setError(String(e));
    }
  }

  async function kickConfirm() {
    if (!window.confirm(`Kick ${target.display_name} from this server?`)) return;
    try {
      await api.kickMember(serverId, targetPk, activeServer?.logServerId ?? null);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function banConfirm(reason: string) {
    setShowBanDialog(false);
    try {
      await api.banMember(serverId, targetPk, activeServer?.logServerId ?? null, reason || undefined);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function timeoutConfirm(untilMs: number, reason: string | null) {
    setShowTimeoutDialog(false);
    try {
      await api.timeoutMember(serverId, targetPk, untilMs, reason);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function removeTimeoutConfirm() {
    try {
      await api.removeTimeout(serverId, targetPk);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  async function blockConfirm() {
    if (!window.confirm(`Block ${target.display_name}? You won't see their messages or DMs.`)) return;
    try {
      await api.blockUser(serverId, targetPk);
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  function copyId() {
    navigator.clipboard.writeText(targetPk).catch(() => {});
    onClose();
  }

  function copyMention() {
    navigator.clipboard.writeText(`@${target.display_name}`).catch(() => {});
    onClose();
  }

  async function toggleBotSubscription() {
    try {
      if (isSubscribed) {
        await api.unsubscribeBot(serverId, targetPk);
        setIsSubscribed(false);
      } else {
        await api.subscribeBot(serverId, targetPk);
        setIsSubscribed(true);
      }
    } catch (e) {
      setError(String(e));
    }
    onClose();
  }

  // Build the items conditionally to compute which separators are needed.
  type Row =
    | { kind: "item"; label: string; onClick: () => void; danger?: boolean }
    | { kind: "submenu"; label: string; submenuKind?: "role" | "language" }
    | { kind: "separator" };
  const rows: Row[] = [];

  rows.push({ kind: "item", label: "View Profile", onClick: viewProfile });
  if (target.is_bot) {
    rows.push({ kind: "separator" });
    rows.push({
      kind: "item",
      label: isSubscribed ? "\u{1F515} Unsubscribe" : "\u{1F514} Notify me",
      onClick: () => void toggleBotSubscription(),
    });
  }
  if (!isSelf && !target.is_bot) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "item", label: "Send Message", onClick: sendMessage });
  }
  // Assign Role shows on your OWN row too (self-assign is allowed; the server
  // bounds non-owners by role hierarchy). Kick/Ban/Timeout stay self-gated below.
  if (canManageRoles && roles.length > 0) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "submenu", label: "Assign Role…  ▶" });
  }
  // Encryption devices (sub-5b G3). Only meaningful inside an ENCRYPTED
  // channel, because a leaf belongs to a channel's group — "which of Alice's
  // devices can read this" has no answer in a plaintext channel. Shown to
  // anyone: seeing who can read a channel you are in is transparency, not a
  // moderator power. The revoke inside is gated by the server, which authorizes
  // it for the owning identity or the server owner and refuses everyone else.
  if (!target.is_bot && e2eeChannelId != null && logServerId) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "item", label: "Encryption devices…", onClick: () => setShowDevices(true) });
  }
  if (!isSelf && !target.is_bot && (canKick || canBan || canTimeout)) {
    rows.push({ kind: "separator" });
    if (canTimeout) {
      if (isTargetTimedOut) {
        rows.push({ kind: "item", label: "Remove timeout", onClick: () => void removeTimeoutConfirm() });
      } else {
        rows.push({ kind: "item", label: "Timeout…", onClick: () => setShowTimeoutDialog(true) });
      }
    }
    if (canKick) rows.push({ kind: "item", label: "Kick", onClick: kickConfirm, danger: true });
    if (canBan) rows.push({ kind: "item", label: "Ban", onClick: () => setShowBanDialog(true), danger: true });
  }
  if (!isSelf && !target.is_bot) {
    rows.push({ kind: "separator" });
    rows.push({ kind: "item", label: "Block", onClick: blockConfirm });
  }
  rows.push({ kind: "separator" });
  rows.push({ kind: "item", label: "Copy ID", onClick: copyId });
  rows.push({ kind: "item", label: "Copy mention", onClick: copyMention });
  rows.push({ kind: "submenu", label: "Set language…  ▶", submenuKind: "language" });

  // Drop leading separator and consecutive separators.
  const cleaned: Row[] = [];
  for (const r of rows) {
    if (r.kind === "separator") {
      if (cleaned.length === 0 || cleaned[cleaned.length - 1].kind === "separator") continue;
    }
    cleaned.push(r);
  }
  if (cleaned.length > 0 && cleaned[cleaned.length - 1].kind === "separator") cleaned.pop();

  // Position at the click — scale-compensated via the shared helper.
  // The member sidebar is on the right, so use "toward-center" to open leftward.
  const menuPosStyle = useClickAnchoredPosition(ref, position, {
    anchor: "toward-center",
    elementW: 180,
    elementH: ref.current?.offsetHeight ?? 0,
  });

  return (
    <>
      {!dialogOpen && (
      <div ref={ref} style={{ ...menuStyle, ...menuPosStyle }}>
        {cleaned.map((row, i) => {
          if (row.kind === "separator") {
            return <div key={`sep-${i}`} style={separatorStyle} />;
          }
          if (row.kind === "submenu") {
            const isLanguage = row.submenuKind === "language";
            const isOpen = isLanguage ? showLanguageSubmenu : showRoleSubmenu;
            const setIsOpen = isLanguage ? setShowLanguageSubmenu : setShowRoleSubmenu;
            return (
              <div
                key={`sub-${i}`}
                onMouseEnter={() => setIsOpen(true)}
                onMouseLeave={() => setIsOpen(false)}
                style={{ position: "relative" }}
              >
                <button
                  style={itemStyle}
                  onClick={() => setIsOpen((s) => !s)}
                >
                  {row.label}
                </button>
                {isOpen && (
                  <div style={submenuStyle}>
                    {isLanguage ? (
                      <SourceLanguagePicker
                        variant="menu"
                        target={translationTarget}
                        value={overrideValue}
                        onChange={async (src) => {
                          try {
                            const settings = await getTranslationSettings();
                            const next = {
                              ...settings,
                              user_language_overrides: {
                                ...settings.user_language_overrides,
                                [targetPk]: src,
                              },
                            };
                            await setTranslationSettings(next);
                            setOverrideValue(src);
                          } catch (e) {
                            // eslint-disable-next-line no-console
                            console.error("[member-menu] set language failed:", e);
                          } finally {
                            setShowLanguageSubmenu(false);
                            onClose();
                          }
                        }}
                        onClear={async () => {
                          try {
                            const settings = await getTranslationSettings();
                            const { [targetPk]: _removed, ...rest } = settings.user_language_overrides;
                            await setTranslationSettings({
                              ...settings,
                              user_language_overrides: rest,
                            });
                            setOverrideValue(null);
                          } catch (e) {
                            // eslint-disable-next-line no-console
                            console.error("[member-menu] clear language failed:", e);
                          } finally {
                            setShowLanguageSubmenu(false);
                            onClose();
                          }
                        }}
                      />
                    ) : (
                      roles.map((r) => {
                        const has = target.role_ids?.includes(r.id) ?? false;
                        return (
                          <button
                            key={r.id}
                            style={itemStyle}
                            onClick={(e) => {
                              e.stopPropagation();
                              void toggleRole(r.id);
                            }}
                          >
                            {has ? "✓ " : "  "}
                            <span style={{ color: r.color ?? "inherit" }}>●</span> {r.name}
                          </button>
                        );
                      })
                    )}
                  </div>
                )}
              </div>
            );
          }
          return (
            <button
              key={`item-${i}`}
              style={{ ...itemStyle, color: row.danger ? "#a00" : itemStyle.color }}
              onClick={row.onClick}
            >
              {row.label}
            </button>
          );
        })}
        {error && (
          <div
            style={{
              color: "#a00",
              fontSize: 10,
              padding: "4px 10px",
              borderTop: "1px solid var(--xp-border, #ccc)",
            }}
          >
            {error}
          </div>
        )}
      </div>
      )}

      {showDevices && e2eeChannelId != null && logServerId && serverId && (
        <E2eeMemberDevices
          serverId={serverId}
          logServerId={logServerId}
          channelId={e2eeChannelId}
          identity={publicKeyToString(target.public_key)}
          memberName={target.display_name}
          onClose={() => { setShowDevices(false); onClose(); }}
        />
      )}

      {showBanDialog && (
        <BanConfirmDialog
          targetName={target.display_name}
          onCancel={() => setShowBanDialog(false)}
          onConfirm={banConfirm}
        />
      )}
      {showTimeoutDialog && (
        <TimeoutDialog
          targetName={target.display_name}
          onCancel={() => setShowTimeoutDialog(false)}
          onConfirm={(untilMs, reason) => void timeoutConfirm(untilMs, reason)}
        />
      )}
      {showProfile && (
        <UserProfilePopup
          member={target}
          roles={roles}
          position={position}
          onClose={() => {
            setShowProfile(false);
            onClose();
          }}
          isSelf={isSelf}
          serverId={serverId}
        />
      )}
    </>
  );
}
