# Frontend hooks

> **File(s):** `client/src/hooks/*.ts`
> **Layer:** Frontend hook
> **Last reviewed:** 2026-09-19

## Purpose

The hooks are where the frontend's *behaviour over time* lives: listening to
server events, deciding when a sealed message may be opened, keeping the local
archive honest. Components render; hooks decide when something happens.

Five of them are mounted once, in `App.tsx`, and never rendered by a component.
**Their mount order is load-bearing** and is commented there:

```tsx
useServerEvents();   // 1. live events -> reducer
useMlsSteward();     // 2. MLS control plane on channel open
useLocalHistory();   // 3. hydrate stored history  ─┐ order matters:
useHistoryRetention();// 4. sweep expired history   │ hydration sets the gate
useSealedDecrypt();  // 5. open what is still sealed ┘ #5 waits on
```

`useSealedDecrypt` must not open a ciphertext in a channel whose stored history
has not been loaded yet: the ratchet key is consumed on open, so re-opening a
message this device already holds fails and caches "couldn't decrypt" over good
history. `useLocalHistory` sets `historyHydrated[channelId]`, which is that gate.

CLAUDE.md's documentation checklist points here for new hooks.

---

## The app-level hooks

### `useServerEvents()`

Subscribes to every `server:*` Tauri event and translates it into reducer
actions. The single biggest surface in the frontend, and the one with the most
per-event judgement: which events notify, which refresh what, which also purge
local state. Two of those side effects are compliance rather than display:

- `server:message_deleted` / `server:message_tombstoned` → `historyPurgeMessage`.
  Server-side a delete removes only the ciphertext, so end to end it means
  nothing unless this device drops its decrypted copy too.
- `server:member_data_deleted` → `MEMBER_LEFT` **and** `historyPurgeAuthor`. This
  is the executed data deletion, distinct from `MemberLeft` (which is reversible
  and leaves the member's messages standing).

Two module-level caches live here. `notifPrefs` is fetched at module load — safe,
because notification prefs are a plain JSON file that does not need the identity.
The own public key is fetched **lazily and never cached as null**: while the
identity is PIN-locked the command answers `null` *successfully*, and caching
that pinned "we don't know who we are" for the whole session is what made every
own-reaction look like someone else's (the stacking-reactions bug).

### `useMlsSteward()`

On opening an E2EE channel, runs the receive-side MLS vertical once so control
events that arrived while the channel was closed — a Welcome addressed to us,
someone else's commit — are applied before the user reads anything. The other
half of the trigger (each incoming `server:mls_control_event`) lives in
`useServerEvents`.

### `useLocalHistory()`

Hydrates `sealedDecrypts` from the local history store when a channel is opened,
then sets `historyHydrated[channelId]`. This is what makes an E2EE channel have
history across restarts at all, and it never opens ciphertext itself.

Marks a channel hydrated **even when hydration fails**. A locked identity or a
missing store must not leave the decrypt gate closed forever — that is the
deadlock shape this codebase keeps rediscovering.

### `useHistoryRetention()`

Sweeps this device's stored history for channels with a `retention_secs` window,
at most once per channel per five minutes. Client-driven by necessity: the
server's retention task is a silent background sweep that broadcasts nothing, and
for an E2EE channel the server is purging ciphertext while the readable copy sits
here. The timestamp is recorded on the attempt, not on success, so a failure
(usually a locked identity) waits out the interval rather than retrying every
render.

### `useSealedDecrypt()`

Opens each sealed row exactly once and writes the result to `sealedDecrypts` and
to the local store. Three rules, each of which has been a bug:

1. **A row we authored is never handed to the decryptor.** A sender has no
   decryption side for its own message; the attempt is guaranteed to fail and
   caches a failure that renders the author's own words as "couldn't decrypt".
   Own messages render from `ownSealedSends` — the text AND attachments recorded
   at send time.
2. **The own-send check runs BEFORE the settled-cache guard.** The server echoes
   the sealed row back before `send_sealed_message` returns, so the decrypt pass
   races ahead of the event hash being recorded; with the guard first, that
   cached failure would be permanent.
3. **One in-flight open per `(server, message, eventHash)`.** A re-render or
   StrictMode double-mount joins the existing promise instead of burning a second
   ratchet key.

It also maps the envelope's three parallel attachment arrays into refs, and
refuses to guess if their lengths disagree: no attachments beats a wrong pairing
of key to filename.

---

## Data-fetching hooks (used by components)

| Hook | Signature | What it does |
|---|---|---|
| `useMemberProfile` | `(serverId, pk, profileHash)` | Avatar + status for one member, cached by `pk:hash`. Keyed by BOTH because a hash alone would let a lying server repoint one member's entry at another's cached profile. |
| `useMessageContext` | `(serverId, channelId, messageId)` | The window of messages around one message, for the search preview pane. Module-level cache keyed `${channelId}:${messageId}`, since a search overlay opens and closes many times over the same rows. |
| `useInvitePreview` | `(link)` | Server name and member counts for an invite link, via the relay fetch proxy. ~60s session cache, matching the relay's own TTL. |
| `useLinkEmbed` | `(url, enabled)` | Rich embed metadata for a URL through the relay. Returns a tagged state (`loading` / `ok` / `unsupported` / `unavailable`) rather than a nullable object, so "we do not proxy this host" and "the fetch failed" stay distinguishable. |
| `useProxiedMedia` | `(url, enabled)` | Media bytes through the relay as a blob URL, revoked on cleanup. The viewer's IP never reaches the origin. |
| `useVoice` | `()` | The whole voice UI surface: join/leave, mute/deafen, per-peer speaking and volume, screenshare. Wraps the `voice_*` command family and the `voice://` event stream. |

---

## Connects to

- `client/src/context/ServerContext.tsx` — every app-level hook dispatches into
  the reducer documented in `frontend-state.md`.
- `client/src/lib/tauri-bridge.ts` — the command wrappers (`frontend-bridge.md`).
- `docs/modules/tauri-bridge.md` — the event names the hooks listen for.

## Gotchas

- **Mount order in `App.tsx` is part of the contract** (see above). Moving
  `useSealedDecrypt` above `useLocalHistory` silently burns ratchet keys.
- **Never cache a "not yet unlocked" answer.** Identity-dependent commands answer
  successfully with `null` while locked; a cache that accepts that value pins it
  for the session.
- A hook that gates other work must mark its gate open **even when it fails**, or
  it deadlocks the thing it gates.
