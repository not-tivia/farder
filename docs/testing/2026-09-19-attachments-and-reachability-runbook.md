# Test Runbook — sealed attachments, and four features that had no way in

**What this is:** the click-through for branch `mesh-rung2-sub6-attachments`
(12 commits on top of `main`). It covers sub-project 6 finished end to end
(files in encrypted channels, voice messages included) plus four features that
existed in the backend with nothing in the UI reaching them.

**Everything here is UNVERIFIED AT RUNTIME.** WSL has no display, so every
frontend path below was built against the type-checker and the audits, not
against a running app. The Rust halves are covered by tests (workspace 969,
client crate 200) and the two-client harness drives a real sealed file over a
real QUIC connection — but none of that proves a button is wired to it.

**You need:** one Windows machine, two identities, ~40 minutes.

---

## Part 1 — Rebuild (order matters, and this branch needs BOTH halves)

This branch changes the protocol, the server and the client, so a stale sidecar
will fail in confusing ways.

1. `git fetch && git checkout mesh-rung2-sub6-attachments && git pull`
2. **Kill every running Farder process** — a running dev session holds the OLD
   sidecar exe and the copy below fails with a file lock. That lock error is the
   useful signal; a silent stale copy would be worse.
3. `cargo build -p farder-server`
4. `.\client\src-tauri\binaries\copy-sidecar.ps1` (from the repo root)
5. `cd client && npm run tauri dev`
6. **Ctrl+Shift+R** once it is up — WebView2 caches stale frontend across
   restarts.

> If `CMAKE_POLICY_VERSION_MINIMUM` bites again: `$env:CMAKE_POLICY_VERSION_MINIMUM=3.5`.

---

## Part 2 — Sealed attachments (the main event)

In an **encrypted** channel, as the owner:

1. **Send a picture.** Paperclip → pick a PNG or JPG → send. It should upload,
   send, and render inline for you. What proves it is sealed rather than
   ordinary: on the server's disk the blob is named `attachment.bin` and its
   bytes are ciphertext (the harness asserts this, but seeing the file is
   reassuring).
2. **The second identity opens it.** They should see the picture, with the
   REAL filename under it — the name travelled inside the ciphertext.
3. **Send a document** (.txt, .pdf). It is not rendered inline: click to open,
   and it saves to Downloads under the sanitized name. Check the saved name
   matches what you sent.
4. **Send a voice message** (the microphone button). This is the W4 path: in an
   encrypted channel it now seals the recording instead of uploading a readable
   WAV. It should play inline for both of you.
5. **Your own attachment must render for YOU.** A sender cannot decrypt their
   own message, so your own file renders from what the client recorded at send
   time. If your own picture shows and theirs does too, both halves work.

**What a failure looks like, and what it means:**
- "This message has a file, but its keys do not line up…" — the cap was
  quarantined server-side; check the server log for `attachment cap mismatch`.
- A refusal message instead of the file — the client-side policy rejected it.
  That is the fail-closed path working; the message says which rule.
- The file downloads but is garbage — that should be impossible (the AEAD fails
  first and you get a refusal). If you ever see it, stop and report it.

**Try to break it (the interesting test):** rename a `.exe` to `.png` and send
it. The sender's own client should refuse it at pick time, before any upload.

---

## Part 3 — The four features that had no way in

Each of these existed in the backend already; only the path from the UI is new.

1. **Unblock someone.** Block a member (member list → right-click), then
   Settings → Privacy & Data → **Blocked Members**. They should be listed, and
   Unblock should remove them. Before this branch there was no way back.
2. **Delete my data.** Settings → Privacy & Data → **Delete My Data**. Request
   it as the SECOND identity (the owner is refused by design — transfer
   ownership first). It should show the scheduled time, ~72 hours out, and
   Cancel should clear it. Do cancel it unless you want that identity gone.
3. **Search an encrypted channel.** Ctrl+K, search for a word you only ever
   typed in an encrypted channel. It should appear, marked "🔒 on this device".
   The server's index cannot see those messages — this searches your local copy.
4. **Hosted servers.** Settings → **Hosted Servers** lists the server processes
   this machine runs, with a Stop button. Start a local server, stop it here,
   confirm members drop, and confirm it is gone from the list.

Also new and quick to eyeball: your own profile card has a colour picker on the
banner (Settings-free: click your own name), and the create-server dialog's
template list now comes from the backend rather than a hardcoded copy.

---

## Part 4 — What to watch for afterwards

- **Retention sweep.** If any channel has a retention window, this client now
  purges its own stored history past that window (at most once per channel per
  5 minutes). Nothing to click; if old messages in a retention channel vanish
  locally, that is it working.
- **A deleted account.** When a deletion request's 72 hours elapse, the server
  now broadcasts it: connected clients drop the member from the roster AND
  purge their local copies of that member's messages. Before, both stayed until
  reconnect.

---

## Known-unverified list (so nothing reads as more tested than it is)

| Area | Verified how |
|---|---|
| Sealed file crosses two clients, no plaintext/name/key in any table or blob | headless harness, real QUIC |
| Server validates + redacts sealed blobs | unit tests |
| Sealed caps materialize (permission + ref count) | harness + unit test, both proven to fail when reverted |
| File policy (names, magic bytes, text) | unit tests incl. hostile suite |
| Everything with a button in it | **nothing — this runbook is that test** |
