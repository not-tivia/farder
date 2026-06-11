# Invite Embeds + Universal Join Gate + Merged Image Menu — Design Spec

**Date:** 2026-06-11
**Status:** Approved (design); ready to plan
**Context:** farder.gg is NOT registered yet (future infra), so in-app paths are
the only working invite paths today. All three items are frontend-only.

## 1. Universal join gate (refactor + paste-path disclosure)

Every way to act on an invite funnels through ONE `JoinConfirmModal` (with its
RELAYED/DIRECT badge from the polish batch).

- **Move `joinConfirm` state into `ServerContext`:** new reducer actions
  `OPEN_JOIN_CONFIRM { link: string }` / `CLOSE_JOIN_CONFIRM`, state field
  `joinConfirmLink: string | null`. `App.tsx` renders the modal from context
  state (its local `useState` goes away); deep-link handling dispatches
  `OPEN_JOIN_CONFIRM` instead of `setJoinConfirm`. `joinFromInvite` stays in
  App.tsx, wired to the modal's onConfirm as today.
- **Paste path gated:** `ConnectDialog`'s join branch and `AddServerModal`'s
  join flow, when the input parses (via `parseInviteLink`) to an **invite**
  (an `address` with an embedded or explicit invite code — i.e. any
  `farder://relay…` link or address+inviteCode), dispatch
  `OPEN_JOIN_CONFIRM(link)` and close themselves instead of connecting
  directly. **Exception:** setup-token flows (first-run owner claim,
  `setup:`/64-hex inputs) keep connecting directly — they are not invites.
- Joining a server you already belong to: unchanged behavior (the confirm
  shows; confirming reconnect/navigates as it does today via joinFromInvite).

## 2. Invite embeds in chat (Discord-style, privacy-free)

A message whose text contains a Farder invite renders an **invite card** below
the text (raw link text remains visible, like Discord).

- **Detection (pure local parsing — no network fetch, no IP leak):** regex scan
  of `message.content` for: `farder.gg/join/<base64url>` (with or without
  https://), `farder://relayd/<hex>/<token>`, `farder://relay/<…>` full form,
  and direct `farder://host:port/<code>`. Each match is validated through
  `parseInviteLink`; non-parsing matches render nothing. Cap: first 3 unique
  invites per message.
- **Card (`InviteEmbed` component):** a bordered panel: title "Server invite",
  the RELAYED/DIRECT badge + one-liner (reuse `.join-relay-note` /
  `.join-relay-badge` classes), and a **Join** button that dispatches
  `OPEN_JOIN_CONFIRM(link)` — the universal gate.
- **V1 limitation (accepted):** no server name / member count on the card
  (that needs an invite-metadata protocol request — future follow-up).
- **Styling:** new classes (`.invite-embed`, `.invite-embed-title`,
  `.invite-embed-join`) styled in ALL THREE theme files via `var(--xp-…)`.
- Rich EXTERNAL embeds (YouTube/fxtwitter) are explicitly OUT OF SCOPE — they
  require a privacy design (metadata fetching leaks viewer IPs; likely proxied
  via the relay). Logged as a standalone future feature.

## 3. Merged image right-click menu

Right-clicking an image shows ONE menu containing the image actions
(Copy Image Link / Save to book / Favorite-less set as shipped) PLUS the
message actions — nothing suppressed, no stacked menus.

- `Message.tsx` builds a `messageActions: { label: string; onClick: () => void }[]`
  array from its existing message context-menu items (reply/edit/delete/etc. —
  exactly the actions its own menu shows for that message) and passes it into
  `AttachmentDisplay` via the `renderRemainingAttachments` closure.
- `AttachmentDisplay`'s image `onContextMenu` calls `e.stopPropagation()` (so
  the message-level menu does not ALSO open) and opens its menu with the image
  items first, a divider (`.context-menu-divider`, styled in all themes if not
  already), then the message actions.
- Left-click on the image keeps today's image-only menu.

## Verification reality

All frontend. Gates: `npx tsc --noEmit`, theme-coverage greps
(`invite-embed`, `context-menu-divider` in all 3 themes), client `cargo build`
untouched. Visuals + interactions UNVERIFIED until the user's Windows pull.

## Out of scope

farder.gg registration/hosting; invite-metadata preview (server name on the
card); rich external embeds (own privacy-aware project); any backend change.
