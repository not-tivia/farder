# Autoplay (trusted senders only)

**What it is:** video from people YOU have put on a list starts by itself, muted,
in chat and in the top-of-screen pop-up. Everything else keeps click-to-play.
**Off by default.** Device-local (`localStorage` key `farder.autoplay`); no server
or log involvement.

**Where:** pure decision in `client/src/lib/autoplay.ts` (`shouldAutoplay`,
`firstDirectMediaUrl`); hooks in `hooks/useAutoplay.ts`; panel
`components/settings/AutoplaySettings.tsx` (Settings -> Autoplay); quick toggle in
the member menu ("Autoplay their media"); chat path `Message.tsx` -> `LinkEmbed`
(`authorPk`) -> `MediaSlot` (`autoplay`) -> a `quiet` player in
`MediaPlayersContext`; pop-up path `useServerEvents#notifyIfWanted` ->
`show_notification(media_url)` -> `NotifyPopup`'s `PopupMedia`.

## Every condition must hold
Autoplay is on, the author is on the list, Data Saver is OFF, and the OS is not
asking for reduced motion. An unknown author never qualifies. The pop-up also
needs its own sub-switch.

## What "autoplay" means here
- **Chat:** a direct/inline video starts once, MUTED, when at least half of it is
  on screen; it pauses when scrolled away and resumes on return; it never
  auto-floats like a clicked video does. The first click on it hands control to
  the person (volume is then remembered as usual). YouTube/Spotify iframes NEVER
  autoplay: they need consent.
- **Pop-up:** only a plain `https` link straight to a video or image file is
  previewed (muted, looping); pages (tweets, YouTube) never are.
- Bytes arrive through the relay-proxied path, so the sender/host never sees the
  viewer's address; nothing is fetched at all unless every condition above holds.
- Encrypted-channel messages do not raise pop-ups, so they never reach this path.

## Tests
`bash client/scripts/run-unit-tests.sh` (`tests/autoplay.test.mjs`).
Player/pop-up behaviour is UNVERIFIED until run on Windows.
