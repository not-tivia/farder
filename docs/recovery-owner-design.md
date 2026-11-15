# Recovery owner: design (2026-11-15; built and tested end to end)

Plain words first. Details for whoever builds it after.

## The problem

A Farder server has ONE owner, and the owner is a key (the owner's account). Today:

- **Account lost** (forgot PIN, new PC): the 24-word recovery phrase restores the same
  key, so ownership comes back. This already works, as long as the owner saved the words.
- **Account stolen** (someone gets the key and the PIN): the thief IS the owner. They can
  delete channels, ban everyone, hand out powers, lock the real owner out. Nothing in the
  server can tell them apart or stop them.
- **Host machine destroyed or wiped**: the server is gone. (Backups now exist; see
  `docs/hosting-on-ubuntu.md` and `crates/farder-server/src/backup.rs`.)

Backups fix the third case, and partly the second (restore to before the damage). They do
NOT fix a stolen owner key: after restoring, the thief still holds the key and can do it
again. The server's identity is derived from the owner key, so there is no way today to
change who the owner is. That missing piece is what this document designs.

## Facts about the current code that shape the design

- The owner is fixed in the server's first record (the genesis) and is a plain field of the
  log's state (`LogState.owner`, `crates/farder-crypto/src/event_log_state.rs`). It is
  checked in about 13 places in the log rules and about 180 in the server.
- The server's identity (`server_id`) is the hash of that first record, so changing the
  owner cannot be done by editing a field: it needs a new kind of record that every
  machine applies the same way.
- Events are signed by a member's devices, and each device has its own chain. A new owner
  needs devices authorized before they can act.

## The idea: a cold key that can always take the server back, slowly

Two keys, with different jobs:

| Key | Where it lives | Powers |
|---|---|---|
| **Owner key** (hot) | the owner's PC, used every day | everything the owner can do today |
| **Recovery key** (cold) | offline: paper, a USB stick, or a second device that is rarely on | can take over the server, but only after a public waiting period |

How a takeover works:

1. The recovery key signs a **claim**: "ownership moves to key N".
2. Every member's app shows it: "The recovery key is taking over this server in 48 hours."
3. The hot owner key CANNOT cancel it. Only the recovery key can cancel its own claim.
4. After the waiting period the claim takes effect: key N is the owner from then on.

Why the hot key must not be able to cancel: if a thief holds the hot key and the real owner
uses the cold key to take the server back, a cancel power would let the thief win.

The price of that rule: the recovery key is now the most valuable thing. Whoever steals it can
claim the server, and the hot key cannot stop them. So it must live offline, and the app
must say so in plain words when it is created.

Rules that keep the two keys from being used against each other:

- The recovery key is set once by the owner key. After that, ONLY the recovery key can
  change it (otherwise a thief with the hot key would just swap in their own).
- The waiting period applies to every claim, and is visible to everyone who is a member.
- While a claim is pending, the owner key keeps working normally (the real owner is not
  frozen), except it cannot change the recovery key or cancel the claim.

## The fallback when there is no recovery key: host-side reset

The person who runs the host machine (the one with a shell on it) is trusted physically.
A command-line action on the host, `farder-server --reset-owner <new key>`, would start a
new history for the same server data under a new owner, announced to members. It needs a
shell on the host, which a thief with only the account key does not have. This is a larger
change (a new genesis carrying members, channels and messages over) and is the second step.

## What has to change, in order

1. **Make ownership changeable in the log rules.** `LogState.owner` becomes the current
   owner; add the claim record and its waiting period (measured in log positions and
   timestamps together, because timestamps are author-chosen and cannot be trusted alone).
   Pure code in `farder-crypto`, fully testable with no network. This is the part that
   needs a security review before anything else is built on it.
2. **Make the server follow it.** Anything that cached the owner (the `owner` setting, the
   owner flag per connection, 180 call sites) reads the current owner, and the server
   refuses a claim made before the key was registered.
3. **A way to create and use the recovery key in the app**: generate it, show it once as
   words to write down, and a "take over" screen that needs the words.
4. **Show a pending claim to everyone**, with the countdown.
5. **Host-side reset** (the fallback above).
6. **Separate, smaller:** a waiting period on destructive actions by the hot key (deleting
   a server's channels in bulk, banning many people at once), cancellable by the recovery
   key. That stops the damage itself while the takeover is waiting.

## Decisions I need from chris before building

- How long is the waiting period? My suggestion: 72 hours (long enough that someone on
  holiday still sees it, short enough to be useful).
- Where should the recovery key live by default? My suggestion: 24 words to write down,
  with no copy kept on the PC.
- Should members be allowed to object? My suggestion: not in the first version; they can
  see it and leave, which is a decision for each of them.

## What this does not solve

A thief who gets BOTH keys. A host machine that is itself taken over (they can read normal
channels and delete the database, which is what backups are for). Metadata. And nothing
here helps if the owner never sets up a recovery key, so the app should ask at creation.


## Build status (2026-11-15)

Built and tested (no network, no screens):

1. **The signed-history rules** (`crates/farder-crypto/src/event_log_state.rs`, tests in
   `recovery_owner_tests`): four new record types, appended to the format: `RecoveryKeySet`,
   `RecoveryClaim`, `RecoveryCancel`, `RecoveryFinalize`. The owner is now whoever the history
   says. Rules pinned by tests (and by deliberately breaking each one): the owner names the
   recovery key once and only the recovery key can change it; the owner key can NOT cancel or
   finish a takeover; the recovery key cannot be banned and survives its device being revoked;
   the new owner must already be a member (a friend's new account joins by an ordinary invite
   first); folding the same history from scratch gives the same owner.
2. **The server side** (`crates/farder-server`): the waiting period is enforced with the
   SERVER'S OWN clock when a takeover is finished (default 72 hours, `--recovery-wait-secs`,
   warned about below 24 hours); the owner is read from the signed history, per request, so a
   thief who is still connected loses owner powers at once and a restart gives the same
   answer; every recovery step goes in the permanent audit log and tells all clients.

3. **The app** (`RecoveryTab.tsx`, five commands, `lib/recovery.ts`): a Recovery tab in server
   settings that every member can open. The owner makes the key and is shown the 24 words once
   (nothing is kept on the computer); anyone sees a pending takeover with its countdown; someone
   holding the words starts, cancels or finishes a takeover.

**Verified on the real stack** (selftest scenario `recovery`, 7 checks, 2026-11-15): two Windows
clients and an Ubuntu server run with `--recovery-wait-secs 30`. The owner makes a recovery key;
a member sees it is set but not the words; the owner (acting as a thief) cannot start a takeover
with made-up words; the member starts one with the real words and both screens show it with a
countdown; finishing early is blocked; after the wait the member finishes it and owns the server;
the old owner, still connected, is refused owner-only actions ("missing MANAGE_CHANNEL
permission") while the new owner succeeds; the audit log holds `recovery_key_set`,
`recovery_claim`, `recovery_finalize`.

Not built yet:

- A banner for EVERY member (today the countdown is inside the Recovery tab, which people must
  open); a pop-up when a takeover starts.
- The host-side reset for a server with no recovery key (`farder-server --reset-owner`).
- The cooling-off period for destructive actions by the everyday key.
- The e2e test only used made-up words for the "thief" case; a VALID phrase that belongs to some
  other server is refused by a check in the app command, covered by reading, not by a test.
- A reminder when the owner has not made a recovery key yet, and a way to rotate it from the app.
