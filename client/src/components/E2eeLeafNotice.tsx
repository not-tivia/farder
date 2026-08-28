import type { NoticeRow } from "../lib/tauri-bridge";

/**
 * An in-channel transparency notice: someone's device gained or lost the ability
 * to read this encrypted channel (spec sub-project 5).
 *
 * Deliberately NOT dismissible and deliberately not a toast. A change to who can
 * read a private channel is the single fact a compromise would most want to slip
 * past you, so it lives in the timeline where it happened, permanently, and it is
 * persisted (sealed) so restarting the app cannot make you miss one.
 *
 * The text names the DEVICE as well as the person: "Alice can read this" is not
 * the claim being made — "a device belonging to Alice can read this" is, and the
 * difference is the entire point when an identity key has been stolen.
 */
export function E2eeLeafNotice({
  notice,
  memberName,
}: {
  notice: NoticeRow;
  /** Display name for the identity, if the roster knows it. */
  memberName?: string;
}) {
  const who = memberName ?? "An unknown member";
  const shortDevice = notice.device.length > 12 ? `${notice.device.slice(0, 12)}…` : notice.device;
  const gained = notice.kind === "gained";

  return (
    <div
      className={`e2ee-leaf-notice${gained ? " e2ee-leaf-notice-gained" : " e2ee-leaf-notice-lost"}`}
      title={`${notice.identity} / ${notice.device}`}
    >
      <span className="e2ee-leaf-notice-icon" aria-hidden="true">
        {gained ? "🔑" : "🚫"}
      </span>
      <span className="e2ee-leaf-notice-text">
        {gained ? (
          <>
            A device of <strong>{who}</strong> can now read this channel
            <span className="e2ee-leaf-notice-device"> ({shortDevice})</span>
          </>
        ) : (
          <>
            A device of <strong>{who}</strong> can no longer read this channel
            <span className="e2ee-leaf-notice-device"> ({shortDevice})</span>
          </>
        )}
      </span>
    </div>
  );
}
