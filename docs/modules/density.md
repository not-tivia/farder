# Message density

**What it does:** lets the person choose how much room each chat message takes:
`roomy`, `normal` (how Farder has always looked, the default), `compact`, `irc`
or `tiny`. One setting for the whole app, stored on this device only
(`localStorage` key `farder.density`). Nothing is sent anywhere.

**Files:** `client/src/lib/density.ts`, `client/src/hooks/useDensity.ts`,
`client/src/components/settings/DensitySettings.tsx` (Settings -> Message density),
the grouping/timestamp change in `client/src/components/Message.tsx`.

## How it works

- `initDensity()` (called from `main.tsx`) injects one `<style id="density-css">`
  and sets `<html data-density="...">`. The stylesheet only ever selects
  `html[data-density="x"] .message...` on the EXISTING message classes and uses
  theme variables (`--xp-panel-bg`) for the one colour it needs, so it works in
  every theme without editing each `theme.css`. (A test fails if it ever contains
  a literal colour.)
- `compact`, `irc` and `tiny` make each message one row (`flex-wrap`): header
  (avatar / name / time) then the text beside it. Attachments, reactions, replies,
  embeds and thread links keep their own line underneath. `irc` and `tiny` hide
  link PREVIEWS (the link stays in the text); `irc` hides avatars and shows
  `[HH:MM] <name> text` in a monospace font; `tiny` hides avatars and the clock.
- `Message.tsx` ignores the "same author as the previous message" folding unless
  the density is `roomy` or `normal` (`foldsRepeatedAuthors`), because a one-row
  message has no header to fold under. In `irc` the time is `shortClock()`
  ("14:05") instead of "Today at 2:05 PM".
- The time in `compact`/`tiny` appears over the right edge on hover (absolutely
  positioned, so hovering never reflows the list).

## Public surface (`lib/density.ts`)

`DENSITIES`, `Density`, `DEFAULT_DENSITY`, `getDensity()`, `setDensity(d)` (saves,
applies, dispatches `farder:density-changed`), `applyDensity(d, root?)`,
`isDensity(v)`, `foldsRepeatedAuthors(d)`, `shortClock(ts)`, `DENSITY_INFO`,
`DENSITY_CSS`, `initDensity()`.

## Gotchas

- Not applied to the pop-up window (it returns before `initDensity`) or to
  anything that does not use the `.message` classes (e.g. the DM panel list uses
  them; the member list does not).
- The `normal` layout has no rules of its own, on purpose: it must stay exactly
  what the theme files draw.
- Tests: `client/tests/density.test.mjs` (logic and the no-literal-colour rule).
  The LOOK is unverified on Windows; see the checklist, Part 8.
