# E2EE Windows Test Runbook

**What this is:** the final check for the encrypted-channels feature (Rung 2). Everything is code-complete and compiled; this is the first real click-through, and it's the only thing that can catch the three surfaces a compile can't: the frontend↔backend command seam at runtime, actual decrypt rendering, and the theme styling.

**You need:** one Windows machine, two identities, about 20 minutes. Do it in order — the build step matters.

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
