# E2EE Windows Test Runbook

**What this is:** the first real click-through for **three** merged-but-never-run sub-projects — 4b (encrypted channels), 7a (local history + at-rest encryption) and 5b-1 (channel keep-alive). It is the only thing that can catch what a compile cannot: the frontend↔backend command seam at runtime, actual decrypt rendering, and theme styling.

**You need:** one Windows machine, two identities, about 30 minutes. Do it in order — the build step matters.

> **Updated 2026-08-28** for 7a + 5b-1. Two things changed that affect this run:
> - **Encrypted channels created before today are DEAD and must be recreated.** Their key store predates at-rest encryption, so the client refuses to resume it. You should see a clear message, not a crash — confirming that IS one of the tests.
> - **Your device key migrates on first unlock** (it used to sit on disk unencrypted). That runs once, automatically. If login itself breaks, that is the migration and it is the highest-severity thing this run can find.

---

## Part 1 — Rebuild (order matters)

1. `git pull` on `main` (tip `a2a1813` or later).
2. **Kill all running Farder processes** — a running `npm run tauri dev` keeps its OLD spawned sidecar.
3. Build the server sidecar:
   ```
   cargo build -p farder-server
   ```
4. Copy the sidecar **from the repo root** (the script uses repo-root-relative paths):
   ```
   .\client\src-tauri\binaries\copy-sidecar.ps1
   ```
5. Start the client:
   ```
   cd client
   npm run tauri dev
   ```
6. Once it's up, do a **hard reload** — WebView2 caches stale frontend across restarts:
   **Ctrl+Shift+R**

> **Why the sidecar rebuild:** 4b touches the server, so an old sidecar will refuse the newer protocol and encrypted channels won't appear at all. This is the #1 way this test silently "fails."

---

## Part 2 — Two identities on one machine

Keep the first instance (the owner) running. Open a **second PowerShell** and run a second identity with a separate data folder:

```powershell
$env:FARDER_DATA = "C:\Users\Deez\farder-identity-2"
$env:WEBVIEW2_USER_DATA_FOLDER = "C:\Users\Deez\farder-webview-2"
.\client\src-tauri\target\debug\farder-client.exe
```

This second instance loads the frontend from the first instance's running dev server, but `FARDER_DATA` gives it its own identity key, PIN, and device. You'll set it up like a fresh install (create identity, choose a different PIN).

Name the identities so you can tell them apart (e.g. "Owner" and "Bob") — you'll be typing in both.

---

## Part 3 — The test scenarios

Do these in order. "Expected" is what correct behavior looks like; if any step diverges, stop and note it.

### 1. Create an encrypted channel
- **Owner:** create a server (fresh), then Settings → Channels → **Create Channel**, check the **Encrypted** box, name it e.g. `#vault`.
- **Expected:** the channel appears with a **🔒** next to its name, and "🔒 Encrypted" in the header. The plaintext explainer shows once at creation.
- **Watch the `tauri dev` console** for `[protocol]` / `[presence]`-style log lines and any red `error`.

### 2. Second identity joins and is auto-added
- **Owner:** create an invite (from the current build — old invite links don't carry the log event).
- **Bob:** join the server via that invite.
- **Expected:** Bob lands in the server, then in `#vault` he goes from a **"waiting for keys"** panel to the normal channel (the owner's client auto-adds him once his key package exists — this can take a few seconds and may need Bob to click into `#vault` first).

### 3. Send an encrypted message both ways
- **Owner** types "hello bob" in `#vault` → **Bob** should see it as real text (not a placeholder).
- **Bob** replies "hi owner" → **Owner** should see it decrypted.
- **Expected:** both sides see plaintext that matches exactly. The composer in `#vault` shows the green accent border and "Encrypted message…" placeholder.

### 4. Fail-closed (the safety check)
- Kill Bob's identity state: close Bob's instance and **delete `C:\Users\Deez\farder-identity-2\servers\<server-id>\mls\`** (or just Bob's `device.key`), then restart Bob and rejoin.
- **Expected:** for any message Bob can no longer decrypt, he sees a **"🔒 couldn't decrypt"** placeholder — never garbage text, never a crash.

### 5. Plaintext still works
- Create a **normal** (unchecked) channel and send a message.
- **Expected:** identical to before — no lock icon, no green border, message renders normally.

---

## What to send back if something fails

For any step that diverges, copy me:
1. Which step + what you saw vs. expected.
2. The last ~10 lines of the `tauri dev` console (especially any `error`, `panic`, or `[protocol]` line).
3. The DevTools console (F12) — any red line, especially one mentioning `invoke`, a command name, or "rejected".

The single most likely thing to break is the **nested-runtime bridge** (`spawn_blocking`) — if creating the encrypted channel hangs or errors immediately in step 1, that's where I'll look first, so a console capture there is gold.

---

## Known caveats (not bugs, don't report)

- **WebView2 stale cache:** if a button "does nothing" or a message posts as plain text, do Ctrl+Shift+R before anything else.
- **Client test flake:** if you ever run `cargo test` in `client/src-tauri`, a pre-existing env race makes some parallel tests flake — run `cargo test -- --test-threads=1`. Irrelevant to this GUI test.
- **`"no published key package"` skip lines** in the console are expected for a member who hasn't opened the channel yet — the owner's client retries once they do.

---

# Part 4 — Added 2026-08-28: history, at-rest, and keep-alive

Do Part 1-3 first (they still apply). These are the newer surfaces.

### 6. Login still works — the device-key migration (7a)

Just unlock normally with your PIN on your **existing** data folder.

- **Expected:** unlocks exactly as before. Behind it, `device.key` is read as raw
  bytes, re-written wrapped, and kept as the SAME key.
- **Check it happened:** the file should no longer be exactly 32 bytes.
  ```powershell
  (Get-Item "$env:APPDATA\..\..\farder\device.key").Length   # or wherever FARDER_DATA points
  ```
- **If unlock fails or the app can't reach your servers, STOP and tell me.** This
  is the one change that touches every launch.

### 7. Old encrypted channels refuse cleanly (7a)

Open an encrypted channel you created **before today**, if you have one.

- **Expected:** a clear refusal — the store predates at-rest encryption and cannot
  be resumed. Recreate the channel to continue.
- **NOT expected:** a crash, a hang, or silently empty/garbled messages.

### 8. **History survives a restart** (7a — the headline test)

This is the single most valuable check in this document, because before 7a the
answer was "no, always".

1. In an encrypted channel, exchange a few messages both ways so both sides have
   read them.
2. **Fully close BOTH instances** (not just the window — make sure the processes
   are gone).
3. Reopen, unlock with your PIN, open the channel.
- **Expected:** the earlier messages are still there **as readable text**.
- **The old, broken behavior:** every previously-read message shows
  `🔒 Encrypted message — couldn't decrypt` under a banner counting them. If you
  see that, 7a's wiring is not working and I need the DevTools console.

### 9. The stored history is actually encrypted (7a — spot check)

With the app closed, search the store for something you typed:

```powershell
Select-String -Path "<FARDER_DATA>\history.db" -Pattern "hello bob" -Encoding utf8
```

- **Expected: no matches.** The message is in there, sealed.
- **If it matches, stop — that is a privacy failure**, not a cosmetic bug.

### 10. Deleting a message really deletes it (7a)

Delete one of your encrypted messages, then restart the app.

- **Expected:** gone, and it stays gone. It must not come back from the local
  store — server-side a delete only removes ciphertext, so the local copy has to
  go too.

### 11. Banning someone doesn't kill the channel (5b-1)

Do this **last**, it removes Bob. In the server with the encrypted channel:

1. **Owner:** ban Bob.
2. **Owner:** send a message in the encrypted channel.
- **Expected:** it sends normally. Behind it the client notices the channel is
  sealed by the ban, removes Bob's stale key, and retries.
- **The old, broken behavior:** the send fails and keeps failing — the ban bricked
  the channel permanently.

### 12. Watch for one specific console line (5b-1)

Throughout, watch the `npm run tauri dev` console for:

```
E2EE: proactive rekey skipped for channel <N>: ...
```

**Report it if you see it.** Key refreshing is deliberately best-effort — it never
fails your send — which means a broken refresh path is otherwise **invisible**, and
this line is the only way it shows up. Sending working is NOT evidence it's fine.

---

## What this run CANNOT test (no UI yet)

Be aware so you don't go looking: `revoke_own_device`, `revoke_member_device`,
`rekey_e2ee_channel` and `reset_e2ee_channel` exist and are registered, but
**nothing in the interface calls them yet** — the buttons are sub-project 5b-2.
There is also no developer console shortcut for them (`withGlobalTauri` is off),
so they genuinely cannot be exercised by hand this run. Their seam stays unproven
until 5b-2 ships the UI.
