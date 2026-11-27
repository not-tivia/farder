import RecoveryBanner from "./RecoveryBanner";
import SavedCopyBanner from "./SavedCopyBanner";

/**
 * Server-wide notices (a recovery takeover in progress, reading a saved copy) in the chat flow under the
 * header. Rendered by EVERY chat view (channel, placeholder, "waiting for keys", thread) so a takeover
 * warning never disappears because of which screen is showing. `.chat-notices:empty` hides it when quiet.
 */
export default function ChatNotices() {
  return (
    <div className="chat-notices">
      <RecoveryBanner />
      <SavedCopyBanner />
    </div>
  );
}
