# Buddy list and chat windows

**What it does:** MSN-Messenger style. An opt-in **buddy list** window lists your
people (direct messages, from every connected server, latest writer first) and the
text channels of each server, with an unread number beside each. **Double-clicking**
an entry opens a small **chat window** for that conversation: the 60 newest messages
and a one-line box to reply. One window per conversation; opening it again brings
the existing one forward. Several can be open with the main window closed.

**Files:** `client/src-tauri/src/buddy.rs` (windows, settings, labels, 4 commands),
`client/src/lib/buddy.ts` (pure: `buildBuddyState`, `buildChatState`, `lineText`,
request parsers), `client/src/hooks/useSmallWindows.ts` (main window: feeds them),
`client/src/components/BuddyList.tsx` and `ChatWindow.tsx` (the two pages, loaded as
`index.html?buddy=1` / `?chat=1&s=<server>&c=<channel>`),
`components/settings/BuddySettings.tsx`, `.buddy-unread` / `.mini-chat*` in every theme.
The list itself reuses the sidebar classes (`channel-sidebar`, `channel-item`...).

## How the pieces talk (events)

| Event | From -> to | Payload |
|---|---|---|
| `buddy:ready` | list -> main | `{}` (list loaded, send state) |
| `buddy:state` | main -> `buddy` only | `{entries: [{server_id, channel_id, label, kind, server_name, unread, encrypted}]}` |
| `chat:ready` | chat -> main | `{label, server_id, channel_id}` |
| `chat:state` | main -> that chat window only | `ChatState` or `null` (conversation gone) |
| `chat:read` | chat -> main | `{label, server_id, channel_id, last_id}` |
| `notify:reply` / `notify:reply-result` | chat <-> main | the same send path as the pop-up |

## Rules it follows

- **The small windows never decide anything.** Every request (`chat:ready`,
  `chat:read`, a reply) comes from another window, so the main window checks the
  label shape, the ids, and that the server and channel exist before acting.
- **Sending is the pop-up's path** (`quickReply.ts` `planReply`): a DM is encrypted
  before it leaves, a channel goes the mesh or legacy way, and an **encrypted channel
  is refused** (the chat window is read-only there: `can_send: false`).
- **Garbling is respected.** `lineText` draws other people's garbled messages
  garbled (same `displayGarble` and seed as `Message.tsx`); your own are as written.
- **Encrypted messages** show their decrypted text from `sealedDecrypts`, or a note
  if not yet decrypted / undecryptable. Nothing is ever decrypted here.
- **Text only in memory, no network.** Attachments appear as `[file: name]`.
- **Idle cost:** `useSmallWindows` only builds text for windows that are open.
- Normal OS title bars and normal focus (unlike the pop-up and the strip, these are
  ordinary windows you type in).

## Untested on a real display

Window sizing, the look, double-click, focus and the live updates are checked by
unit tests of the pure parts only; see `docs/testing/2026-11-07-features-checklist.md` Part 11.
