# Hosting a Farder server on Ubuntu

The community server (`farder-server`) is a normal command-line program. It does not need
a screen, so it runs on a plain Ubuntu machine (a home server or a VPS). The Farder app on
Windows connects to it the same way it connects to a server on another Windows PC.

## What has been tested (2026-11-15)

- The server builds and runs on Ubuntu 24.04 and its full test suite passes there
  (1010 tests).
- It starts headless, listens, and prints a one-time **setup token** on first run.
- A Windows Farder client claimed an Ubuntu-hosted server with that token and became its
  owner, invited a second Windows client, and the two chatted (self-test scenario
  `ubuntu`, 4 checks). Same machine, different operating system, direct connection.

## Not tested yet

- Running it as a boot-time service: the unit's start, restart-after-crash and stop were run as a user service; the full system install (`User=farder`) is untested.
- Over the internet, through a router, or through the relay (`--relay`) from Ubuntu.
- A release build, a long uptime, and restart after a crash.
- Copying backups to ANOTHER machine automatically (a backup is a plain folder; copying it off the host is manual for now).

## Steps

1. Build: `cargo build --release -p farder-server`
2. Run: `./target/release/farder-server --bind 0.0.0.0:4435 --name "My Server"`
   (the port is UDP; open it with `sudo ufw allow 4435/udp`). The name is saved in the database, so later
   starts keep it without `--name` (giving `--name` again renames the server), and a `--read-only` copy made
   from this database shows the same name.
3. First run prints `use this token instead: <64 characters>`. In the Farder app choose
   **Add Server, Join a Server** and paste `farder://<server address>:4435/setup:<token>`.
   Whoever does this first becomes the owner, so do it yourself before telling anyone else
   the address.
4. Make invites from the app as usual.
5. To keep it running without a terminal, use `deploy/server/farder-server.service`.

## Backups and restore (built and tested 2026-11-15)

```
# automatic: one at start, then every 6 hours, keeping the newest 14
farder-server ... --backup-dir /var/backups/farder
# one now (works while the server runs)
farder-server ... --backup-now /var/backups/farder
# restore (server stopped). Refuses to overwrite unless --force; with --force the old
# data is moved aside to *.before-restore-<time>, never deleted.
farder-server ... --restore-from /var/backups/farder/farder-backup-<time> [--force]
```

A backup is a folder with the database snapshot, the uploaded files, the server's stable
identity, and a checksum that is verified before any restore. Copy that folder to another
machine (rsync, a USB stick, a cloud folder) to survive a dead disk.

Tested for real: two Windows clients chatted on an Ubuntu-hosted server, a backup was taken
while it ran, the server's data was destroyed, the backup restored, and the server came back
as the same server: both clients reconnected on their own with their history, membership
and (for A) ownership, and chatted again. A backup is a copy: if an attacker with the
owner's key wipes things and a later automatic backup runs, that backup holds the damage,
which is why several are kept. See `docs/recovery-owner-design.md` for the plan for that case.

## Owner lost the account and made no recovery key: reset the owner from the host

Stop the server, then run (the new owner must already be a member; copy their key from the
members list, `vk_` followed by 64 hex characters):

```
farder-server ... --reset-owner vk_<member key>
```

It makes a backup first (in `farder-pre-reset-backups` beside the data folder), keeps the old signed
history under `events_before_reset_<time>` in the database, and starts a new history owned by that
member. Start the server again. Channels and plain messages stay; everyone except the new owner
rejoins with a fresh invite. End-to-end encrypted channels lose their membership. If the server
HAS a recovery key, use the takeover in Server Settings > Recovery instead (`--force` overrides).

## Protection against a stolen account: the bulk-damage brake

By default one account (the owner too) can make 5 destructive changes (deleting channels, categories or
roles, kicking or banning, removing bots or webhooks) in 10 minutes; the next one is refused with "slow
down" and recorded in the audit log. If you really need to clean up in bulk, wait it out, or start the
server with `--destructive-budget 50` (and back to the default afterwards); `--destructive-budget 0` turns
it off. It is kept in memory, so restarting the server gives everyone a fresh budget.
