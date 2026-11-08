# Windows test checklist - everything on `features-skins-notify-garble`

**What this is:** one pass over everything built on this branch since the
2026-09-19 handoff. None of it has been seen running. WSL has no display, so the
code is covered by tests (Rust, plus a Node runner for the frontend logic) but
**every button, window and sound below is unverified until you do this.**

**You need:** the Windows machine, ~60 minutes, and a **second identity** to send
you messages (the same way you did for the earlier attachments runbook). A second
identity is needed for Parts 3, 4, 5 and 6.

**How to report:** for anything that is not as described, send me (1) what you
clicked, (2) what you expected (copy the "should" line), (3) what happened, (4) a
screenshot, and (5) the last ~30 lines of the `npm run tauri dev` console. Lines
starting `[notify]`, `[audio]`, `[presence]` are mine and are useful. "It did not
work" with nothing else makes me guess.

Tick the boxes as you go. **Order matters:** Parts 1-3 are the riskiest, so they
come first.

---

## Part 0 - Get the branch and rebuild

The branch is local to the WSL repo and **not pushed** (it has 12 commits over `main`). Either:

- **A.** Tell me "push it" and I push it as a branch (not `main`), then in PowerShell:
  `git fetch` then `git checkout features-skins-notify-garble`.
- **B.** Without pushing, in `C:\Users\Deez\farder`:
  `git fetch \\wsl.localhost\Ubuntu\home\deez\farder features-skins-notify-garble:features-skins-notify-garble`
  then `git checkout features-skins-notify-garble`. (If Git complains about
  "dubious ownership", it is the WSL path; tell me the message.)

Then, **after closing every Farder window and quitting it from the tray**:

```powershell
cd C:\Users\Deez\farder
cargo build -p farder-server
.\client\src-tauri\binaries\copy-sidecar.ps1
cd client
npm run tauri dev
# when the window is up: Ctrl+Shift+R
```

The server and protocol changed, so the **sidecar must be rebuilt** (not just the
client).

- [ ] The sidecar `farder-server-x86_64-pc-windows-msvc.exe` has **today's date/time**
      after the copy. *If the date did not move, stop: nothing below is meaningful.*
- [ ] If the copy fails with a file-lock error, a Farder process is still running
      (check the tray, then Task Manager). Quit it and copy again.
- [ ] If CMake complains: `$env:CMAKE_POLICY_VERSION_MINIMUM=3.5` and rebuild.
- [ ] Optional: `git stash list` - if the old "line-ending churn" stash is there,
      `git stash drop` it (do not pop it).

---

## Part 1 - First launch, tray and themes

- [ ] **A window opens.** It should show within a few seconds. *If nothing appears
      and there is no tray icon, tell me at once: the main window now starts hidden
      and is shown during setup.*
- [ ] **A tray icon is visible** near the clock (check the `^` overflow arrow). It
      should be the Farder icon, not blank.
- [ ] **One title bar, not two**, and the window drags, maximizes, restores and
      resizes from all four edges and corners (carried over from the last handoff).
- [ ] Settings (gear) -> **Appearance** lists **Velvet, Nocturne and Candy** as well
      as the original three. Switch to each:
  - [ ] Text is readable everywhere you look (sidebar, chat, member list, settings).
  - [ ] No leftover Discord-blurple (in Velvet/Nocturne) or Hello-Kitty pink (in
        Candy) buttons.
  - [ ] Server Settings tabs look like tabs, not bare grey buttons.

## Part 2 - Close to tray, start in tray, start with Windows

- [ ] Click the title-bar **X**. *Should:* the window disappears, the tray icon stays,
      and Farder is still in Task Manager.
- [ ] The **first** time, a themed pop-up appears at the top-centre: "Farder is still
      running". *(Once ever. To see it again, quit Farder and delete the line
      `"tray_hint_shown": true` from `C:\Users\Deez\.farder\settings.json`.)*
- [ ] **Left-click** the tray icon: the window comes back in front.
- [ ] **Right-click** the tray icon: menu with "Open Farder" and "Quit Farder".
- [ ] **Quit Farder** from the menu. *Should:* everything is gone from Task Manager -
      no `farder-client`, no `msedgewebview2` belonging to it, and no `farder-server`
      if you were hosting. *(A leftover process here is a real bug.)*
- [ ] Settings -> **Startup & tray**: untick "Closing the window keeps Farder running".
      Click X. *Should:* Farder quits for real. Re-tick it afterwards.
- [ ] Tick "Start in the tray", quit, launch again. *Should:* no window, only the tray
      icon; left-click opens it. Untick it afterwards.
- [ ] "Start Farder when I sign in to Windows" should be **greyed out with an
      explanation** in `tauri dev` (it only works in an installed build). That is
      expected.
- [ ] **Known gap, just observe:** with Farder sitting in the tray, launch it again.
      A second copy will start. Tell me what you see (this is the single-instance
      problem; do not treat it as a new bug).

## Part 3 - Pop-ups (the riskiest part)

Leave Farder **minimized** (not in the tray yet) with a channel open. Have the
second identity post in a **normal (non-encrypted)** channel, then in a **DM**.

- [ ] **A pop-up appears at the top-centre of your main screen**, in your current
      theme, over whatever you are doing. The title names the sender and channel;
      the body is the message.
- [ ] **It never steals focus.** Open Notepad and keep typing while a message
      arrives. *Should:* every keystroke keeps going into Notepad, nothing is lost.
      *(If your typing is interrupted even once, that is the most important bug on
      this list.)*
- [ ] It disappears by itself after about 6 seconds, **holds while the mouse is over
      it**, and the **x** dismisses it. **Clicking the card** opens Farder.
- [ ] Send 4-5 messages quickly: they stack (at most 4), nothing overlaps or runs
      off screen.
- [ ] When Farder is **in front**, a message in the channel you are looking at, or in
      another channel of the *same* server, raises **no** pop-up (expected: you can see
      the unread marker). A message in a *different server* does raise one, and so does
      any message while Farder is minimized, hidden in the tray or behind another window.
- [ ] If your display scale is 125% (or 150%), the pop-up is still centred and not
      clipped.
- [ ] **Encrypted channels never pop up** (by design). Confirm: a message in one
      raises nothing.

### Pop-up privacy options - Settings -> Notifications -> Pop-ups

- [ ] **What a pop-up may show** - set each and have the other identity post a
      message with a distinctive word:
  - [ ] *Who sent it, where, and the message* - shows all three.
  - [ ] *Only who sent it* - "New message from <name>"; **the word and channel are
        nowhere**.
  - [ ] *Nothing* - just "New message"; **no name, no channel, no word**. Take a
        screenshot and check it for the word.
- [ ] **Pop-up style = System notification**: a Windows toast appears instead, and it
      respects the content setting above.
- [ ] **Pop-up style = No pop-ups at all**: nothing appears. In Task Manager, expand
      Farder: there should be **one fewer** `msedgewebview2` process than with the
      themed style (the pop-up window is no longer kept). Switch back and it returns.

### Reply from the pop-up

Set style = Farder pop-up, content = full, "Let me reply by typing into a pop-up" ticked.

- [ ] Each pop-up has a **Reply** button.
- [ ] Click it. *Should:* a text box appears and you can type straight away (the
      window takes focus only now).
- [ ] Type a line, press **Enter**. *Should:* "Sending..." then "Sent", the card goes
      away, and **the other identity sees your message** in that channel.
- [ ] **After sending, focus goes back** to whatever you were using (type in
      Notepad: it should receive the keys).
- [ ] **Esc** (or clicking elsewhere) cancels the box and the card times out again.
- [ ] Reply to a **DM** pop-up: the other identity reads it normally in the DM.
- [ ] Turn "Let me reply..." off: the Reply button is gone. Set content to
      *Nothing*: the Reply button is also gone. Set it back.
- [ ] With the **garbler on** ("Speak as: Bark"), a pop-up reply goes out garbled.
- [ ] Try replying while **timed out** (if you can arrange it): you should see the
      server's error in the pop-up rather than "Sent".

## Part 4 - Autoplay (trusted people only)

Settings -> **Autoplay**. Have the second identity post a link straight to a short
**.mp4** file (and a normal image link, and a YouTube link).

- [ ] Off by default; with it off, the mp4 shows the usual poster and "Play".
- [ ] Right-click the second identity in the member list -> **Autoplay their media**
      (it turns autoplay on and adds them). They appear in Settings -> Autoplay.
- [ ] A new mp4 from them **starts by itself, muted**, when it is on screen, **pauses
      when you scroll away** and resumes when you scroll back, and does **not**
      float over the chat.
- [ ] Clicking the video hands control to you (you can unmute).
- [ ] The same mp4 from someone **not** on the list does **not** autoplay.
- [ ] The **YouTube** link does **not** autoplay.
- [ ] Turn **Data Saver** on: autoplay stops (and the screen says why).
- [ ] With "Also show it in the pop-up" ticked and content = full: a minimized-Farder
      pop-up for an mp4 link from a trusted person shows a **muted looping preview**
      inside the card, and the window grows to fit. With content = *Only who*, no
      preview.
- [ ] Windows **Settings -> Accessibility -> Visual effects -> Animation effects = Off**:
      nothing autoplays (Farder honours the system's reduce-motion setting). Turn it
      back on afterwards.

## Part 5 - Garbler and Powers

### Speak as (your own words)

- [ ] Settings -> **Speech Garbler**: pick Bark, tick the box. A badge appears above
      the message box ("Speaking as: Bark... Turn off").
- [ ] Send "hello there https://farder.xyz @name": words become barks; **the link and
      the @mention are untouched**. A slash command (e.g. `/poll`) still works.
- [ ] Turn it off with the badge button.

### Powers (Server Settings -> Powers - owner only, mesh server)

- [ ] The **Powers** tab exists for you (the owner) and **not** for the other identity.
- [ ] Tick **Garble** for the second identity. A row appears in **Server Settings -> Audit
      Log**: "<you> gave <them> the "garble" power". Untick it: "took the "garble"
      power from <them>".
- [ ] Re-tick it for the next tests.

### Moderation garble

- [ ] As the second identity (now holding Garble): right-click a member ->
      **Garble...** exists. As an ordinary member without it (and with no consent given
      to them), it does **not**.
- [ ] Start a moderation garble on someone for 2 messages. They send 3 messages.
      *Should:* messages 1-2 appear **garbled with "garbled by a moderator"** to
      everyone else; message 3 is normal. The garbled person sees **their own words**
      with a note.
- [ ] The garbled person sees a **banner above their message box**, with **no Stop
      button** for a moderation garble.
- [ ] **Audit Log** shows "started garbling <name> (bark, next 2 messages)".
- [ ] The owner can't be garbled (the menu entry may be missing or the attempt refused).

### Consent garble

- [ ] As the target: Settings -> Speech Garbler -> "Let someone garble me": pick the
      other identity, limit 5, **Allow**.
- [ ] As that person (holding no Garble power): right-click the target -> Garble... now
      exists and is the *consent* kind (up to 5 messages). Start it. (If they also hold the
      Garble power, the dialog offers both kinds as radio buttons.)
- [ ] Their messages show **"garbled (they agreed to this)"**. The target's banner
      has a **Stop** button that works. "Take back" removes the permission.
- [ ] **No** audit row appears for the consent kind (it is play, not an act of authority).

## Part 6 - Sound (protect your ears: set the ceiling FIRST)

Settings -> **Sound**. Before testing anything loud, tick "Never let anything be
louder than this" and set it to about **-24 dB**, and keep the volume slider low.

- [ ] **Voice call** with the second identity: have them speak normally, then **shout
      or tap the mic hard**. *Should:* the shout is held down to roughly the same
      loudness as normal speech instead of jumping out. Turn the ceiling **off** and
      repeat once at low volume to hear the difference.
- [ ] Slide **Volume** up to 150% with the ceiling on: normal speech gets louder but
      a shout still does not exceed the ceiling.
- [ ] A **loud video** in chat (a clip with a loud peak) and a **voice message**: also
      held down, and they still play (no silence, no crackle). Reload the app after
      changing the setting if the first one seems unaffected, and tell me.
- [ ] **YouTube/Spotify** players are *not* limited (expected; the screen says so).
- [ ] Settings persist after quitting and relaunching.

## Part 7 - Idle usage (the one I could not measure)

Quit Farder; start it normally; sign in; open the main window; **do not touch it**.

```powershell
powershell -ExecutionPolicy Bypass -File C:\Users\Deez\farder\scripts\measure_idle.ps1 -Seconds 180
```

- [ ] Run once with the window **in front**, then once **minimized to the taskbar**,
      then once **hidden in the tray**. Send me all three outputs.
- [ ] *Roughly what I hope for:* average CPU under ~1% of one core, almost no samples
      at/above 1%, memory not climbing during the run. If CPU is steady above that
      with nothing happening, that is a bug I want to find.
- [ ] Repeat the "in the tray" run with Settings -> Notifications -> Pop-ups set to
      **No pop-ups at all** and compare memory (the pop-up window is the difference).

---

## Leftovers from earlier work (still unseen)

- [ ] **Plaintext delete stays deleted:** delete a message in a normal channel, quit
      and relaunch (or restart the server): it must not come back. (Fixed in code and
      pinned by tests; never observed.)
- [ ] The **attachments runbook** (`2026-09-19-attachments-and-reachability-runbook.md`)
      is still the biggest unrun block; start with its Part 2.

---

## Known gaps - do not report these as new bugs

- No **single-instance** guard (a second launch starts a second copy).
- **Start with Windows** only works in an installed build, not `tauri dev`.
- **YouTube/Spotify** iframes can't be volume-limited or autoplayed.
- **Encrypted channels** raise no pop-ups, so there is nothing to reply to from them.
- **Edits** to a garbled message are not re-garbled.
- Only the **owner** has the Powers screen; a delegated holder can use their power but
  can't hand it on from a screen.
- Three existing `farder-mls` store tests fail on `main` too (flaky, unrelated), and
  five client unit tests fail only when run in parallel.

## When you are done

Send me the unticked boxes with the details above. Items that pass need no
explanation - a plain list of what passed and what did not is the most useful
thing you can give me.
