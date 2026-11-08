# Speech garbler (sender-chosen)

**What it is:** a personal setting, "Speak as...", that turns the words of YOUR
messages into barks, meows, moos, mumbles, scrambles, shuffles or a custom phrase
just before they are sent. Ported from the screen censor app's `garble.py`.

**Where:** pure logic in `client/src/lib/garble.ts`; the on/off choice in
`client/src/lib/garbleSettings.ts` (`localStorage` key `farder.garble`, this
device only); the panel in `components/settings/GarbleSettings.tsx` (Settings ->
Speech Garbler); applied in `MessageInput.tsx#handleSend`.

## Rules (all deliberate)

- It runs on the sender's device, BEFORE sealing/sending, so it works identically
  in plaintext, encrypted and DM channels and the server never sees the original.
- Everyone, including the sender, sees the garbled words. There is no hidden mode.
- While on, a badge above the message box says so, with a one-click **Turn off**.
- Slash commands (`/poll`, ...) are handled before garbling and are never changed.
- Left alone: `https://` links, `@mentions`, `:emoji:` tokens, `<...>` tokens,
  inline code and fenced code, plus all spacing and punctuation.
- Not applied to: edits to existing messages, voice messages, file-only sends.
- Nothing is stored or sent about the setting; the settings are device-local.

## Tests

`bash client/scripts/run-unit-tests.sh` (node's built-in runner, `tests/garble.test.mjs`).

## Garbling someone else (moderation and consent)

Same words-on-screen effect, but another person starts it, so it is built on the
signed log and is deliberately hard to misuse.

**Events** (`farder-crypto`): `GarbleSet { target, mode, phrase, messages,
consensual }`, `GarbleCleared { target }`, `GarbleConsent { controller,
max_messages }`. The fold (`LogState`) is the authority; it records, for each
message accepted while a rule runs on its author, a `GarbleMark` (mode, phrase,
who set it, moderation or not). The server stores that mark with the message row
(`messages.garble`) and returns it in broadcasts and history (`MessageInfo.garble`).

**Two kinds, never confusable:**

| | Moderation | With consent |
|---|---|---|
| Who may start | holder of the `garble` capability (the owner implicitly) | the person the target named in `GarbleConsent` |
| Limit | at most 50 messages | at most what the target allowed (max 500) |
| Target | anyone but the owner | anyone who agreed |
| Who can end it | setter, any `garble` holder | setter, any `garble` holder, **and the target (kill switch)** |
| Labelled | "garbled by a moderator" on every message | "garbled (they agreed to this)" |
| Audit log | yes, `garble_set` / `garble_cleared` (moderation category, kept) | no (play, not authority) |

**Rules that hold in both:**
- Nothing is rewritten. The words stay in the log; each viewer's client DRAWS them
  garbled (seeded by the event hash, so every viewer and every re-render agree).
- The author always sees their own words as written, with a note that others see
  them garbled. A banner above their message box says a rule is running.
- The limit is a MESSAGE COUNT: event timestamps are author-chosen, so a clock
  cannot bound anything.
- A new rule replaces the old; leaving or being banned ends every rule and consent
  involving the member; revoking consent ends that controller's rule.
- Only people a rule concerns can learn it exists (`GetGarbleState`).

**It is a display effect, not a security control.** The original text is still in
the log and on the server for plaintext channels; a modified client could show it.

**Granting the power:** Server Settings -> Powers (owner only) ticks `garble` (and
`invite` / `kick` / `ban`) on or off per member; see `PermissionRevoked` for the
take-back. **Not built:** edits to a garbled message are not re-garbled. Sealed (encrypted-channel) messages ARE marked
and drawn garbled once decrypted.
