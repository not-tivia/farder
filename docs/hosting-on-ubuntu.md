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

- Running it as a boot-time service (the unit file below is untested).
- Over the internet, through a router, or through the relay (`--relay`) from Ubuntu.
- A release build, a long uptime, and restart after a crash.
- Backups and restore (not built yet: see `docs/mesh-hosting-status.md`).

## Steps

1. Build: `cargo build --release -p farder-server`
2. Run: `./target/release/farder-server --bind 0.0.0.0:4435 --name "My Server"`
   (the port is UDP; open it with `sudo ufw allow 4435/udp`).
3. First run prints `use this token instead: <64 characters>`. In the Farder app choose
   **Add Server, Join a Server** and paste `farder://<server address>:4435/setup:<token>`.
   Whoever does this first becomes the owner, so do it yourself before telling anyone else
   the address.
4. Make invites from the app as usual.
5. To keep it running without a terminal, use `deploy/server/farder-server.service`.

Your server's data lives in the database file and the storage folder named by `--db` and
`--storage-dir`. Back those up together (copy them while the server is stopped) until a
real backup feature exists.
