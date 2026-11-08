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

## Not built here

An ADMIN garbling another member (a moderation action) is a separate feature
needing a signed log event, a permission and an audit row; it is not part of this.
