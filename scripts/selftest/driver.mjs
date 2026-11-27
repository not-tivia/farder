// Two-client self-test driver. Run it WITH WINDOWS NODE (the debugging ports only
// listen on Windows' own localhost):
//
//   node scripts\selftest\driver.mjs core
//
// It expects start-pair.ps1 to be running two Farder windows (A on 9222, B on
// 9223) with empty data folders. It clicks through the real screens like a person
// and prints one line per check: PASS / FAIL with what it saw. Exit code 1 if
// anything failed. Nothing here touches your real identity.
import { chromium } from "playwright-core";
import fs from "node:fs";

const PORT_A = Number(process.env.PORT_A || 9222);
const PORT_B = Number(process.env.PORT_B || 9223);
const PIN = "1234";
const results = [];
let shotN = 0;

function log(status, name, detail = "") {
  results.push({ status, name, detail });
  console.log(`${status.padEnd(4)} ${name}${detail ? "  -- " + detail : ""}`);
}
const watched = [];
async function check(name, fn) {
  try { const d = await fn(); log("PASS", name, d ?? ""); return true; }
  catch (e) {
    log("FAIL", name, String(e.message || e).split("\n")[0]);
    for (const c of watched) for (const l of c.logs.slice(-12)) console.log(`      [${c.who} log] ${l}`);
    return false;
  }
}
async function shot(page, tag) {
  const dir = process.env.SHOTS || "selftest-shots";
  fs.mkdirSync(dir, { recursive: true });
  const f = `${dir}/${String(++shotN).padStart(2, "0")}-${tag}.png`;
  await page.screenshot({ path: f }).catch(() => {});
  return f;
}

async function attach(port, who) {
  const c = await attach1(port, who);
  watched.push(c);
  return c;
}
async function attach1(port, who) {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const ctx = browser.contexts()[0];
  // The main window is the page that is not one of the small windows (?buddy, ?chat, ?notify, ?strip).
  let page;
  for (let i = 0; i < 60 && !page; i++) {
    page = ctx.pages().find((p) => !/[?&](buddy|chat|notify|strip)=/.test(p.url()) && /index\.html|localhost|tauri/.test(p.url() + "tauri"));
    if (!page) await new Promise((r) => setTimeout(r, 500));
  }
  if (!page) throw new Error(`no main window found for ${who}`);
  page.setDefaultTimeout(15000);
  page.on("pageerror", (e) => console.log(`[${who}] page error: ${e.message.split("\n")[0]}`));
  // Keep the app's own [chat]/[buddy] log lines so a failure can show what it did.
  const logs = [];
  page.on("console", (m) => { const t = m.text(); if (/^\[(chat|buddy|notify)\]/.test(t)) logs.push(`${new Date().toISOString().slice(11, 19)} ${t.slice(0, 160)}`); });
  return { browser, ctx, page, who, logs };
}

async function createIdentity(c, name) {
  const p = c.page;
  await p.getByText("Create identity").first().waitFor({ timeout: 30000 });
  const pins = p.locator('input[aria-label="PIN entry"]');
  await pins.nth(0).fill(PIN);
  await pins.nth(1).fill(PIN);
  // The second PIN box creates the identity by itself once its 4th digit is in.
  // Making the identity (key derivation) is slow in a debug build.
  await p.getByRole("button", { name: /saved it/ }).click({ timeout: 120000 });
  await p.getByPlaceholder("Display name").fill(name);
  await p.getByRole("button", { name: "Continue" }).click();
  await p.getByText("Create a Server").waitFor({ timeout: 60000 });
}

async function createServer(c, name) {
  const p = c.page;
  await p.getByText("Create a Server").click();
  await p.getByPlaceholder("My Awesome Server").fill(name);
  await p.getByRole("button", { name: "Next" }).click();
  await p.getByRole("button", { name: "Create Server" }).click();
  // The main layout appears with the default #general channel.
  await p.getByText("general").first().waitFor({ timeout: 60000 });
}

// The first-run "Translation" dialog covers the app until it is answered.
async function skipTranslation(c) {
  const skip = c.page.getByRole("button", { name: /Skip/ });
  if (await skip.waitFor({ timeout: 6000 }).then(() => true, () => false)) await skip.click();
}

async function makeInvite(c) {
  await skipTranslation(c);
  const p = c.page;
  await p.getByTitle("Create Invite").click();
  await p.getByRole("button", { name: "Create Invite Link" }).click();
  const input = p.locator(".invite-link-display input");
  await input.waitFor({ timeout: 20000 });
  const link = await input.inputValue();
  if (!/^(farder:\/\/|https:\/\/farder\.xyz\/join\/)/.test(link)) throw new Error("unexpected link: " + link.slice(0, 60));
  await p.locator(".modal-close").first().click();
  return link;
}

async function joinWithLink(c, link) {
  const p = c.page;
  await p.getByText("Join a Server").click();
  await p.getByPlaceholder("farder://server/invite-code").fill(link);
  await p.getByRole("button", { name: "Join", exact: true }).click();
  // A link that goes through the relay asks "You've been invited... Join it?" first.
  const confirm = p.getByText(/Join it\?/);
  if (await confirm.waitFor({ timeout: 8000 }).then(() => true, () => false)) {
    await p.getByRole("button", { name: "Join", exact: true }).last().click();
  }
  await p.getByText("general").first().waitFor({ timeout: 60000 });
  await skipTranslation(c);
}

async function sendText(c, text) {
  const p = c.page;
  const box = p.getByPlaceholder(/Type a message/);
  await box.fill(text);
  await box.press("Enter");
}
async function seesText(c, text, ms = 20000) {
  await c.page.locator(".message-content", { hasText: text }).first().waitFor({ timeout: ms });
}

async function core() {
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  const stamp = Date.now().toString(36);

  if (!(await check("A: create identity and reach the welcome screen", () => createIdentity(A, "Alice")))) return shot(A.page, "A-identity-fail");
  if (!(await check("B: create identity and reach the welcome screen", () => createIdentity(B, "Bob")))) return shot(B.page, "B-identity-fail");
  if (!(await check("A: create a server (#general appears)", () => createServer(A, "SelfTest " + stamp)))) return shot(A.page, "A-server-fail");
  let link = null;
  if (!(await check("A: create an invite link", async () => { link = await makeInvite(A); return link.slice(0, 40) + "..."; }))) return shot(A.page, "A-invite-fail");
  if (!(await check("B: join from the invite", () => joinWithLink(B, link)))) return shot(B.page, "B-join-fail");
  await check("A: send a line, A sees it", async () => { await sendText(A, "hello from A " + stamp); await seesText(A, "hello from A " + stamp); });
  await check("B: sees A's line", () => seesText(B, "hello from A " + stamp));
  await check("B: send a line, B sees it", async () => { await sendText(B, "hello from B " + stamp); await seesText(B, "hello from B " + stamp); });
  await check("A: sees B's line", () => seesText(A, "hello from B " + stamp));
  fs.writeFileSync(process.env.STATE || "selftest-state.json", JSON.stringify({ stamp }));
  await shot(A.page, "A-final");
  await shot(B.page, "B-final");
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

async function unlock(c) {
  const p = c.page;
  await p.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 60000 });
  await p.locator('input[aria-label="PIN entry"]').first().fill(PIN);
  // ANY visible "general": in the small layout the full sidebar's copy is hidden and comes first.
  await p.waitForFunction(() => [...document.querySelectorAll(".channel-item")].some((e) => e.innerText.includes("general") && e.getBoundingClientRect().width > 0), null, { timeout: 90000 });
}

// Run AFTER `core`, with both windows quit and started again WITHOUT -Fresh:
// the same identities must come back, still in the server, history intact.
async function relaunch() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  await check("A: unlock after relaunch, still in the server", () => unlock(A));
  await check("B: unlock after relaunch, still in the server (no re-invite)", () => unlock(B));
  // The app reopens on the server used last; go to the first one, where the history is.
  for (const c of [A, B]) {
    await c.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  await check("A: earlier history is still there", () => seesText(A, "hello from B " + stamp));
  await check("B: earlier history is still there", () => seesText(B, "hello from A " + stamp));
  await check("A: send after relaunch, B sees it", async () => { await sendText(A, "after relaunch A " + stamp); await seesText(B, "after relaunch A " + stamp); });
  await check("B: send after relaunch, A sees it", async () => { await sendText(B, "after relaunch B " + stamp); await seesText(A, "after relaunch B " + stamp); });
  await shot(A.page, "A-relaunch");
  await shot(B.page, "B-relaunch");
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Wait until a window whose address matches `re` exists (small windows are separate pages).
async function pageMatching(c, re, ms = 30000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const p = c.ctx.pages().find((x) => re.test(x.url()));
    if (p) { p.setDefaultTimeout(15000); return p; }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error("no window matching " + re);
}

// The buddy list and a chat window: run when both clients are up and unlocked
// (after `core`, or after `relaunch`).
async function buddy() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  // After a relaunch the PIN screen may be showing.
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  // A stays on whatever it was showing (after the DM scenario that is the DM, which
  // is the interesting case for live updates); B looks at #general to read it.
  await B.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  let list, chat;
  if (!(await check("A: open the buddy list", async () => {
    await A.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("open_buddy_list"));
    list = await pageMatching(A, /[?&]buddy=1/);
    await list.locator(".channel-item", { hasText: "general" }).first().waitFor({ timeout: 30000 });
  }))) { if (list) await shot(list, "A-buddy-fail"); return; }
  await shot(list, "A-buddy");
  if (!(await check("A: clicking #general opens a chat window that shows the history", async () => {
    await list.locator(".channel-item", { hasText: "general" }).first().click();
    chat = await chatWithHead(A, "#general");
    await chat.locator(".mini-chat-text", { hasText: "hello from A " + stamp }).first().waitFor({ timeout: 30000 });
  }))) { if (chat) await shot(chat, "A-chat-fail"); return; }
  await shot(chat, "A-chat");
  await check("A: the chat window has a visible typing box at the bottom", async () => {
    const box = await chat.locator(".mini-chat-input").boundingBox();
    const vp = chat.viewportSize() ?? (await chat.evaluate(() => ({ width: innerWidth, height: innerHeight })));
    if (!box) throw new Error("no typing box");
    if (box.y + box.height > vp.height + 1) throw new Error(`box is off screen (y=${Math.round(box.y)} h=${Math.round(box.height)} window h=${vp.height})`);
  });
  await check("A: a line typed in the chat window shows there and reaches B", async () => {
    const box = chat.locator(".mini-chat-input");
    await box.fill("from the mini window " + stamp);
    await box.press("Enter");
    const t0 = Date.now();
    try { await chat.locator(".mini-chat-text", { hasText: "from the mini window " + stamp }).first().waitFor({ timeout: 20000 }); }
    catch (e) {
      await shot(chat, "A-chat-after-send");
      const lines = await chat.locator(".mini-chat-line").allInnerTexts();
      const notes = await chat.locator(".mini-chat-note").allInnerTexts();
      throw new Error("the chat window shows " + JSON.stringify(lines) + " notes " + JSON.stringify(notes) + " (wanted: from the mini window " + stamp + ")");
    }
    const t1 = Date.now();
    await seesText(B, "from the mini window " + stamp, 90000);
    return `shown in the chat window after ${Math.round((t1 - t0) / 1000)}s, in B after ${Math.round((Date.now() - t0) / 1000)}s`;
  });
  await check("A: a line B sends appears live in A's chat window", async () => {
    await sendText(B, "live to the mini " + stamp);
    const t0 = Date.now();
    await chat.locator(".mini-chat-text", { hasText: "live to the mini " + stamp }).first().waitFor({ timeout: 90000 });
    return `after ${Math.round((Date.now() - t0) / 1000)}s`;
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// The chat window whose heading is `head` (several chat windows can be open).
async function chatWithHead(c, head, ms = 30000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    for (const p of c.ctx.pages().filter((x) => /[?&]chat=1/.test(x.url()))) {
      const t = await p.locator(".mini-chat-head").first().innerText().catch(() => "");
      if (t.trim() === head) { p.setDefaultTimeout(15000); return p; }
    }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error("no chat window titled " + head);
}

// Direct messages: A messages B from the member list, then the same conversation
// through A's buddy list. Run when both clients are up (after `core`).
async function dm() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  await check("A: B's display name (Bob) replaces \"Anonymous\" in A's member list", async () => {
    const t0 = Date.now();
    await A.page.locator(".member-item", { hasText: "Bob" }).first().waitFor({ timeout: 120000 });
    return `after ${Math.round((Date.now() - t0) / 1000)}s from the start of this check`;
  });
  if (!(await check("A: open a direct message with Bob (member list -> Message)", async () => {
    await A.page.locator(".member-item", { hasText: "Bob" }).first().click();
    await A.page.getByRole("button", { name: "Message", exact: true }).click();
    // Opening a DM takes a few seconds: wait for the DM view itself (header "Bob" + Pop Out).
    await A.page.getByRole("button", { name: "Pop Out" }).waitFor({ timeout: 40000 });
  }))) return shot(A.page, "A-dm-open-fail");
  await shot(A.page, "A-dm-open");
  await check("A: send a direct message, A sees it", async () => { await sendText(A, "dm one " + stamp); await seesText(A, "dm one " + stamp); });
  await check("B: the direct message reaches B (readable, not ciphertext)", async () => {
    try {
      await B.page.locator(".dm-item, .channel-item", { hasText: "Alice" }).first().click({ timeout: 30000 });
    } catch (e) {
      // Say what B's backend knows, so the failure points at the right layer.
      const info = await B.page.evaluate(async () => {
        const inv = window.__TAURI_INTERNALS__.invoke;
        const out = [];
        for (const s of await inv("get_saved_servers")) {
          try { out.push((await inv("list_dms", { serverId: s.id })).map((d) => d.participant.display_name)); } catch (x) { out.push("list_dms failed: " + x); }
        }
        return out;
      }).catch((x) => "evaluate failed: " + x);
      await shot(B.page, "B-no-dm");
      throw new Error("B has no DM entry in the sidebar; B's backend list_dms = " + JSON.stringify(info));
    }
    try { await seesText(B, "dm one " + stamp, 30000); } catch (e) {
      const info = await B.page.evaluate(async () => {
        const inv = window.__TAURI_INTERNALS__.invoke;
        const s = (await inv("get_saved_servers"))[0];
        const dms = await inv("list_dms", { serverId: s.id });
        const hist = await inv("fetch_history", { serverId: s.id, channelId: dms[0].channel.id });
        return { dmChannel: dms[0].channel.id, backendLines: hist.map((m) => String(m.content).slice(0, 30)) };
      }).catch((x) => "evaluate failed: " + x);
      await shot(B.page, "B-dm-view");
      const dom = await B.page.locator(".message-content").allInnerTexts();
      throw new Error("B's DM view shows " + JSON.stringify(dom.slice(-3)) + "; backend has " + JSON.stringify(info));
    }
  });
  let list, chat;
  await check("A: the buddy list shows Bob under PEOPLE", async () => {
    await A.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("open_buddy_list"));
    list = await pageMatching(A, /[?&]buddy=1/);
    await list.locator(".channel-item.dm-item", { hasText: "Bob" }).first().waitFor({ timeout: 30000 });
  });
  if (!list) return;
  await shot(list, "A-buddy-people");
  await check("A: clicking Bob opens a chat window with the direct message", async () => {
    await list.locator(".channel-item.dm-item", { hasText: "Bob" }).first().click();
    chat = await chatWithHead(A, "Bob");
    await chat.locator(".mini-chat-text", { hasText: "dm one " + stamp }).first().waitFor({ timeout: 30000 });
  });
  if (!chat) return;
  await shot(chat, "A-dm-chat");
  await check("A: a line typed in the DM chat window reaches B", async () => {
    const box = chat.locator(".mini-chat-input");
    await box.fill("dm from mini " + stamp);
    await box.press("Enter");
    await chat.locator(".mini-chat-text", { hasText: "dm from mini " + stamp }).first().waitFor({ timeout: 30000 });
    await seesText(B, "dm from mini " + stamp, 30000);
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Edit and delete your own message; the other person must see both. Run when both
// clients are up (after `core`).
async function edit() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  const orig = "edit me " + stamp, edited = "edited " + stamp;
  await check("A: send a line to edit", async () => { await sendText(A, orig); await seesText(B, orig); });
  await check("A: edit it (right-click, Edit Message); both see the new text, B no longer sees the old", async () => {
    await A.page.locator(".message", { hasText: orig }).first().click({ button: "right" });
    await A.page.locator(".context-menu-item", { hasText: "Edit Message" }).click();
    const box = A.page.locator(".message-edit-input");
    await box.fill(edited);
    await box.press("Enter");
    await seesText(A, edited);
    await seesText(B, edited);
    await B.page.locator(".message-content", { hasText: orig }).first().waitFor({ state: "detached", timeout: 15000 });
    await shot(B.page, "B-edited");
  });
  await check("A: delete it (right-click, Delete Message); it disappears for both", async () => {
    await A.page.locator(".message", { hasText: edited }).first().click({ button: "right" });
    await A.page.locator(".context-menu-item.delete").click();
    await A.page.locator(".message-content", { hasText: edited }).first().waitFor({ state: "detached", timeout: 20000 });
    await B.page.locator(".message-content", { hasText: edited }).first().waitFor({ state: "detached", timeout: 20000 });
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// A second server that is NOT relayed ("Direct - same network only"): the kind a
// friend on the same network would join, and the kind whose port used to move
// around between launches. A creates it, B joins by the invite, both chat.
async function direct() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const name = "Direct " + stamp;
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  let link = null;
  if (!(await check("A: create a second, direct (non-relayed) server", async () => {
    await A.page.getByTitle("Add Server").click();
    await A.page.getByText("Create a Server").click();
    await A.page.getByPlaceholder("My Awesome Server").fill(name);
    await A.page.getByRole("button", { name: "Next" }).click();
    await A.page.getByText(/Direct .{1,3} same network only/).click();
    await A.page.getByRole("button", { name: "Create Server" }).click();
    await A.page.locator(`.server-icon[title="${name}"]`).waitFor({ timeout: 90000 });
    await A.page.locator(".channel-item", { hasText: "general" }).first().waitFor({ timeout: 30000 });
  }))) return shot(A.page, "A-direct-create-fail");
  if (!(await check("A: create an invite for it (a direct link, not a relay link)", async () => {
    link = await makeInvite(A);
    // Web links wrap the real address in base64: decode it to see where it points.
    const m = link.match(/farder\.xyz\/join\/([A-Za-z0-9_-]+)/);
    const target = m ? Buffer.from(m[1], "base64url").toString("utf8") : link;
    if (/relay/.test(target)) throw new Error("expected a direct link, it points at " + target.slice(0, 60));
    return "points at " + target.slice(0, 30);
  }))) return shot(A.page, "A-direct-invite-fail");
  if (!(await check("B: join the direct server from the invite", async () => {
    await B.page.getByTitle("Add Server").click();
    await B.page.getByText("Join a Server").click();
    await B.page.getByPlaceholder("farder://server/invite-code").fill(link);
    await B.page.getByRole("button", { name: "Join", exact: true }).click();
    const confirm = B.page.getByText(/Join it\?/);
    if (await confirm.waitFor({ timeout: 8000 }).then(() => true, () => false)) await B.page.getByRole("button", { name: "Join", exact: true }).last().click();
    await B.page.locator(`.server-icon[title="${name}"]`).waitFor({ timeout: 90000 });
  }))) return shot(B.page, "B-direct-join-fail");
  await check("A and B chat on the direct server", async () => {
    await A.page.locator(`.server-icon[title="${name}"]`).click();
    await B.page.locator(`.server-icon[title="${name}"]`).click();
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await sendText(A, "direct hi from A " + stamp);
    await seesText(B, "direct hi from A " + stamp, 30000);
    await sendText(B, "direct hi from B " + stamp);
    await seesText(A, "direct hi from B " + stamp, 30000);
  });
  await shot(B.page, "B-direct");
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// After a restart (same data): the direct server must still be there for both, with
// no new invite, and chat must work again.
async function directRelaunch() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const name = "Direct " + stamp;
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 20000 }).then(() => true, () => false)) await unlock(c);
  }
  await check("A: the direct server is still in the list after a restart", () => A.page.locator(`.server-icon[title="${name}"]`).waitFor({ timeout: 60000 }));
  await check("B: the direct server is still in the list after a restart (no re-invite)", () => B.page.locator(`.server-icon[title="${name}"]`).waitFor({ timeout: 60000 }));
  await check("A and B chat on the direct server after the restart", async () => {
    await A.page.locator(`.server-icon[title="${name}"]`).click();
    await B.page.locator(`.server-icon[title="${name}"]`).click();
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await seesText(A, "direct hi from B " + stamp, 60000);
    await sendText(A, "direct after restart " + stamp);
    await seesText(B, "direct after restart " + stamp, 60000);
  });
  await shot(A.page, "A-direct-relaunch");
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Unread markers: the dot on a server you are not looking at, and the highlight on
// a conversation you are not looking at. Run after `direct` (needs two servers + a DM).
async function unread() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const first = `.server-icon[title="SelfTest ${stamp}"]`;
  const second = `.server-icon[title="Direct ${stamp}"]`;
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  await check("B looks at the direct server; A posts in the first one; B's first server shows an unread dot", async () => {
    await B.page.locator(second).click();
    await A.page.locator(first).click();
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await sendText(A, "unread ping " + stamp);
    await B.page.locator(`${first} .server-unread-dot`).waitFor({ timeout: 30000 });
    await shot(B.page, "B-unread-dot");
  });
  await check("B opens that server: the dot goes away and the line is there", async () => {
    await B.page.locator(first).click();
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await seesText(B, "unread ping " + stamp, 20000);
    await B.page.locator(`${first} .server-unread-dot`).waitFor({ state: "detached", timeout: 15000 });
  });
  await check("A messages B in their DM while B reads #general: B's DM entry is highlighted as unread", async () => {
    await A.page.locator(".channel-item.dm-item", { hasText: "Bob" }).first().click();
    await A.page.getByRole("button", { name: "Pop Out" }).waitFor({ timeout: 20000 });
    await sendText(A, "dm unread " + stamp);
    await B.page.locator(".channel-item.dm-item.unread", { hasText: "Alice" }).waitFor({ timeout: 30000 });
    await shot(B.page, "B-dm-unread");
  });
  await check("B opens the DM: the highlight goes away and the message is readable", async () => {
    await B.page.locator(".channel-item.dm-item", { hasText: "Alice" }).first().click();
    await seesText(B, "dm unread " + stamp, 20000);
    await B.page.locator(".channel-item.dm-item.unread").waitFor({ state: "detached", timeout: 15000 });
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// The notification pop-up (a separate window): a message arrives while B is not
// looking at that channel; the pop-up must show it, and a reply typed in the pop-up
// must reach A. Run after `core`.
async function popup() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const first = `.server-icon[title="SelfTest ${stamp}"]`;
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  await A.page.locator(first).click().catch(() => {});
  await A.page.locator(".channel-item", { hasText: "general" }).first().click();
  // B looks at the other (direct) server, so a line in the first one is not what B is viewing.
  await B.page.locator(`.server-icon[title="Direct ${stamp}"]`).click();
  let pop;
  if (!(await check("a message arrives: B's pop-up window shows it", async () => {
    await sendText(A, "popup ping " + stamp);
    pop = await pageMatching(B, /[?&]notify=1/, 30000);
    await pop.locator(".notify-popup-card", { hasText: "popup ping " + stamp }).first().waitFor({ timeout: 30000 });
    await shot(pop, "B-popup");
  }))) return;
  await check("the pop-up has a Reply button, and a reply typed there reaches A", async () => {
    await pop.locator(".notify-popup-reply-btn").first().click();
    const box = pop.locator(".notify-popup-reply-input");
    await box.fill("popup reply " + stamp);
    await box.press("Enter");
    await seesText(A, "popup reply " + stamp, 30000);
  });
  await check("B is looking at the SAME channel but Farder is in the background (focus simulated): the pop-up still shows", async () => {
    // The driver makes every page report "has focus", so the one answer the app asks
    // (document.hasFocus) is overridden to say "no". Everything after that is real.
    await B.page.locator(first).click();
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await B.page.evaluate(() => { document.hasFocus = () => false; });
    try {
      await sendText(A, "background ping " + stamp);
      await pop.locator(".notify-popup-card", { hasText: "background ping " + stamp }).first().waitFor({ timeout: 30000 });
    } finally {
      await B.page.evaluate(() => { delete document.hasFocus; });
    }
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Every theme: switch to it on A, check it really applied, that the message text and
// the sidebar text are readable against their background, that nothing sticks out
// sideways, and keep a screenshot to look at. Run after `core` (needs messages on screen).
async function themes() {
  const A = await attach(PORT_A, "A");
  if (await A.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(A);
  await A.page.locator(".server-icon:not(.add-server)").first().click().catch(() => {});
  await A.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  const list = await A.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("list_themes"));
  const original = await A.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("get_active_theme").then((t) => t.id));
  console.log("themes:", list.map((t) => t.id).join(", "));
  for (const t of list) {
    await check(`theme ${t.id}: applies, readable, nothing sticks out`, async () => {
      await A.page.evaluate(async (id) => {
        const inv = window.__TAURI_INTERNALS__.invoke;
        const css = await inv("load_theme_css", { id });
        document.getElementById("active-theme").textContent = css;
        document.documentElement.dataset.theme = id;
      }, t.id);
      await new Promise((r) => setTimeout(r, 600));
      const r = await A.page.evaluate((id) => {
        const lum = (rgb) => { const m = rgb.match(/[\d.]+/g).map(Number); const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }; return 0.2126 * f(m[0]) + 0.7152 * f(m[1]) + 0.0722 * f(m[2]); };
        const bgOf = (el) => { for (let e = el; e; e = e.parentElement) { if (getComputedStyle(e).backgroundImage !== "none") return null; /* a gradient or picture: cannot be measured here, look at the screenshot */ const c = getComputedStyle(e).backgroundColor; const m = c.match(/[\d.]+/g); if (m && (m.length < 4 || Number(m[3]) > 0.5)) return c; } return "rgb(255, 255, 255)"; };
        const contrast = (el) => { const bg = bgOf(el); if (bg === null) return null; const a = lum(getComputedStyle(el).color), b = lum(bg); return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05); };
        const msg = document.querySelector(".message-content");
        const side = document.querySelector(".channel-item:not(.active)");
        const av = document.querySelector(".message-avatar");
        return {
          css: document.getElementById("active-theme").textContent.length,
          dataset: document.documentElement.dataset.theme === id,
          msg: msg ? contrast(msg) : null,
          side: side ? contrast(side) : null,
          avatar: av ? contrast(av) : null,
          overflow: document.documentElement.scrollWidth > window.innerWidth + 2,
        };
      }, t.id);
      await shot(A.page, "theme-" + t.id);
      if (!r.dataset || r.css < 2000) throw new Error("theme did not apply: " + JSON.stringify(r));
      if (r.msg !== null && r.msg < 3) throw new Error("message text hard to read, contrast " + r.msg.toFixed(1));
      if (r.side !== null && r.side < 3) throw new Error("sidebar text hard to read, contrast " + r.side.toFixed(1));
      if (r.avatar !== null && r.avatar < 3) throw new Error("avatar letters hard to see, contrast " + r.avatar.toFixed(1));
      if (r.overflow) throw new Error("page is wider than the window");
      return `contrast message ${r.msg?.toFixed(1)}, sidebar ${r.side?.toFixed(1)}, avatar ${r.avatar?.toFixed(1)}`;
    });
  }
  await A.page.evaluate(async (id) => {
    const css = await window.__TAURI_INTERNALS__.invoke("load_theme_css", { id });
    document.getElementById("active-theme").textContent = css;
    document.documentElement.dataset.theme = id;
  }, original);
  await A.browser.close().catch(() => {});
}

// A small real PNG (16x16, solid orange), written to the shots folder; returns { path, fileName, png }.
async function orangePng(stamp, label = "orange") {
  const { deflateSync } = await import("node:zlib");
  const crcT = Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
  const crc = (b) => { let c = 0xffffffff; for (const x of b) c = crcT[(c ^ x) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
  const chunk = (t, d) => { const len = Buffer.alloc(4); len.writeUInt32BE(d.length); const td = Buffer.concat([Buffer.from(t), d]); const cs = Buffer.alloc(4); cs.writeUInt32BE(crc(td)); return Buffer.concat([len, td, cs]); };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(16, 0); ihdr.writeUInt32BE(16, 4); ihdr[8] = 8; ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.concat(Array.from({ length: 16 }, () => Buffer.from([255, 140, 0])))]);
  const png = Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(Buffer.concat(Array.from({ length: 16 }, () => row)))), chunk("IEND", Buffer.alloc(0))]);
  const dir = process.env.SHOTS || "selftest-shots";
  fs.mkdirSync(dir, { recursive: true });
  const fileName = `${label}-${stamp}.png`;
  const path = (await import("node:path")).resolve(dir, fileName);
  fs.writeFileSync(path, png);
  return { path, fileName, png };
}

// Drop a file on a window (the same event Windows sends when you drag one in), wait until it is staged,
// then send it with a caption.
async function dropAndSend(c, path, fileName, caption) {
  await c.page.evaluate((p) => window.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "tauri://drag-drop", payload: { paths: [p], position: { x: 400, y: 400 } } }), path);
  const staged = await c.page.getByText(fileName).first().waitFor({ timeout: 30000 }).then(() => true, () => false);
  if (!staged) {
    const errs = await c.page.locator(".error-text").allInnerTexts().catch(() => []);
    const previews = await c.page.locator(".attachment-preview").allInnerTexts().catch(() => []);
    throw new Error(`${c.who}: the dropped file never showed as staged (previews on screen: ${JSON.stringify(previews).slice(0, 80)})` + (errs.length ? ` (screen says: ${errs.join(" | ").slice(0, 120)})` : ""));
  }
  await sendText(c, caption);
  await seesText(c, caption, 30000);
}

// The picture in the message with `caption` has actually loaded on c's screen.
async function pictureLoads(c, caption) {
  const msg = c.page.locator(".message", { hasText: caption }).first();
  await msg.locator("img:not(.message-avatar img)").last().waitFor({ timeout: 30000 });
  const ok = await msg.locator("img").evaluateAll((imgs) => imgs.some((i) => i.naturalWidth >= 8 && !i.classList.contains("message-avatar")));
  if (!ok) throw new Error("the picture element is there but did not load (naturalWidth 0)");
}

// A file attachment: drop a picture on A's window (the same event Windows sends when
// you drag a file in), send it, and B must receive it AND be able to fetch and show
// it. Run after `core`.
async function attach_file() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    await c.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  const { path, fileName } = await orangePng(stamp);
  await check("A: drop a picture on the window, it is staged, then sent with a caption", async () => {
    await dropAndSend(A, path, fileName, "with a file " + stamp);
    await shot(A.page, "A-file-sent");
  });
  await check("B: receives the message with the file, and the picture actually loads", async () => {
    await seesText(B, "with a file " + stamp, 30000);
    await pictureLoads(B, "with a file " + stamp);
    await shot(B.page, "B-file");
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// The sender's own garbler: with it on, what A types reaches B as garbled words.
// Run after `core`.
async function garble() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    await c.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  const setG = (enabled) => A.page.evaluate((en) => {
    localStorage.setItem("farder.garble", JSON.stringify({ enabled: en, mode: "bark", phrase: "" }));
    window.dispatchEvent(new CustomEvent("farder:garble-changed"));
  }, enabled);
  try {
    await setG(true);
    await check("garbler on: what A types reaches B as garbled words, not the original", async () => {
      await sendText(A, "secret plan quietly " + stamp);
      // The newest message in B's channel is A's; it must exist and differ from what was typed.
      await B.page.waitForFunction(() => document.querySelectorAll(".message-content").length > 0);
      await new Promise((r) => setTimeout(r, 4000));
      const texts = await B.page.locator(".message-content").allInnerTexts();
      const last = texts[texts.length - 1] || "";
      await shot(B.page, "B-garbled");
      if (/secret|plan|quietly/.test(last)) throw new Error("B still sees the original words: " + last.slice(0, 60));
      if (last.trim().length === 0) throw new Error("B sees an empty message");
      return "B sees: " + last.slice(0, 40);
    });
  } finally {
    await setG(false);
  }
  await check("garbler off again: a normal line arrives unchanged", async () => {
    await sendText(A, "plain again " + stamp);
    await seesText(B, "plain again " + stamp, 30000);
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// The edge strip (a one-line bar docked to the screen edge, replaces the pop-up when on).
// Run after `direct` (B watches the second server). Turns the strip back off at the end.
async function strip() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const first = `.server-icon[title="SelfTest ${stamp}"]`;
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  // A call that never comes back is a finding, not a reason to hang the whole run.
  const setStrip = (enabled) => Promise.race([
    B.page.evaluate((en) => window.__TAURI_INTERNALS__.invoke("set_edge_strip", { settings: { enabled: en, dock: "bottom", align: "center", width: 700, always_visible: true } }), enabled),
    new Promise((_, rej) => setTimeout(() => rej(new Error("set_edge_strip did not answer in 25s (the app is hung)")), 25000)),
  ]);
  try {
    await A.page.locator(first).click();
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await B.page.locator(`.server-icon[title="Direct ${stamp}"]`).click();
    let st;
    if (!(await check("strip on: the strip window appears", async () => {
      await setStrip(true);
      st = await pageMatching(B, /[?&]strip=1/, 30000);
      await st.locator(".edge-strip").waitFor({ timeout: 20000 });
      await shot(st, "B-strip-idle");
    }))) return;
    await check("a message arrives: the strip shows it (and who/where from)", async () => {
      await sendText(A, "strip ping " + stamp);
      await st.locator(".edge-strip", { hasText: "strip ping " + stamp }).waitFor({ timeout: 30000 });
      await shot(st, "B-strip-message");
    });
  } finally {
    await setStrip(false).catch(() => {});
  }
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// A moderator garbling someone else (a moderation garble, as the server owner A):
// B's next lines are drawn garbled for OTHER people, while B keeps their own words.
// Then the owner stops it and B is normal again. Run after `core`.
async function adminGarble() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    await c.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  const ids = await A.page.evaluate(async () => {
    const inv = window.__TAURI_INTERNALS__.invoke;
    const s = (await inv("get_saved_servers"))[0];
    const info = await inv("get_server_info_v2", { serverId: s.id });
    const members = await inv("get_members", { serverId: s.id });
    const bob = members.find((m) => m.display_name === "Bob");
    const hex = bob.public_key.bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
    return { serverId: s.id, logServerId: info.server_id, bob: "vk_" + hex };
  });
  const call = (page, cmd, args) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  await check("owner A starts a moderation garble on Bob (next 2 messages)", async () => {
    await call(A.page, "garble_set", { serverId: ids.serverId, logServerId: ids.logServerId, target: ids.bob, mode: "bark", phrase: "", messages: 2, consensual: false });
  });
  await check("Bob types a line: Bob still sees his own words, A sees it garbled", async () => {
    await sendText(B, "moderated words " + stamp);
    await seesText(B, "moderated words " + stamp, 30000);                 // the author keeps their own copy
    await new Promise((r) => setTimeout(r, 4000));
    const texts = await A.page.locator(".message-content").allInnerTexts();
    const last = texts[texts.length - 1] || "";
    await shot(A.page, "A-sees-garbled");
    if (/moderated|words/.test(last)) throw new Error("A still sees the original words: " + last.slice(0, 60));
    return "A sees: " + last.slice(0, 40);
  });
  await check("owner A stops it: Bob's next line is normal for A", async () => {
    await call(A.page, "garble_clear", { serverId: ids.serverId, logServerId: ids.logServerId, target: ids.bob });
    await new Promise((r) => setTimeout(r, 1500));
    await sendText(B, "after the stop " + stamp);
    await seesText(A, "after the stop " + stamp, 30000);
  });
  // The consent version: A may only garble Bob if Bob agreed first.
  const alice = await A.page.evaluate(async (sid) => {
    const m = (await window.__TAURI_INTERNALS__.invoke("get_members", { serverId: sid })).find((x) => x.display_name === "Alice");
    return "vk_" + m.public_key.bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
  }, ids.serverId);
  await check("without Bob's consent, a consensual garble is refused", async () => {
    let refused = false;
    try { await call(A.page, "garble_set", { serverId: ids.serverId, logServerId: ids.logServerId, target: ids.bob, mode: "bark", phrase: "", messages: 1, consensual: true }); }
    catch (e) { refused = true; }
    if (!refused) throw new Error("it was accepted without consent");
  });
  await check("Bob consents (up to 3 messages); then A can garble him, and his next line shows garbled to A", async () => {
    // B reaches the same server under its own address string (it joined by a different link form).
    const bServer = await B.page.evaluate(async (name) => (await window.__TAURI_INTERNALS__.invoke("get_saved_servers")).find((x) => x.name === name)?.id, "SelfTest " + stamp);
    await call(B.page, "garble_consent", { serverId: bServer, logServerId: ids.logServerId, controller: alice, maxMessages: 3 });
    await new Promise((r) => setTimeout(r, 2000));
    await call(A.page, "garble_set", { serverId: ids.serverId, logServerId: ids.logServerId, target: ids.bob, mode: "bark", phrase: "", messages: 1, consensual: true });
    await sendText(B, "consented words " + stamp);
    await new Promise((r) => setTimeout(r, 4000));
    const texts = await A.page.locator(".message-content").allInnerTexts();
    const last = texts[texts.length - 1] || "";
    if (/consented|words/.test(last)) throw new Error("A still sees the original: " + last.slice(0, 60));
    return "A sees: " + last.slice(0, 40);
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Close-to-tray: closing B's window must NOT quit the app (it is a messenger: it stays
// in the tray), B must still receive messages while hidden, and a pop-up must show.
// DESTRUCTIVE for B (no tray click is automated), so the runner puts it last.
async function tray() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const { execFileSync } = await import("node:child_process");
  const root = process.env.DATAROOT || `${process.env.USERPROFILE}\\farder-selftest-data`;
  const pids = fs.readFileSync(`${root}\\pids.txt`, "utf8").split(/\r?\n/).filter(Boolean);
  const bPid = pids[1];
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  for (const c of [A, B]) {   // both look at the first server's #general
    await c.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  const ps = (cmd) => execFileSync("powershell.exe", ["-NoProfile", "-Command", cmd], { encoding: "utf8" }).trim();
  await check("closing B's window leaves the app running (hidden in the tray, not quit)", async () => {
    ps(`(Get-Process -Id ${bPid}).CloseMainWindow() | Out-Null`);
    await new Promise((r) => setTimeout(r, 4000));
    const alive = ps(`if (Get-Process -Id ${bPid} -ErrorAction SilentlyContinue) { 'alive' } else { 'gone' }`);
    if (alive !== "alive") throw new Error("the app quit when its window was closed");
    const vis = await B.page.evaluate(() => document.visibilityState).catch(() => "unreachable");
    return "process " + alive + ", page " + vis;
  });
  await check("while hidden in the tray, B still receives a message and its pop-up shows (focus simulated)", async () => {
    await B.page.evaluate(() => { document.hasFocus = () => false; });   // the driver forces "has focus"; the real window is hidden
    await sendText(A, "while hidden " + stamp);
    await seesText(B, "while hidden " + stamp, 30000);                     // it arrived while B's window is hidden
    const pop = await pageMatching(B, /[?&]notify=1/, 30000);
    await pop.locator(".notify-popup-card", { hasText: "while hidden " + stamp }).first().waitFor({ timeout: 40000 });
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Delegated powers: the owner hands Bob the "garble" and "invite" powers; Bob can use
// them only while he holds them; the owner takes them back. Run after `core`.
async function powers() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    await c.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  const call = (page, cmd, args) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  const info = async (page, name) => page.evaluate(async (nm) => {
    const inv = window.__TAURI_INTERNALS__.invoke;
    const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
    const i = await inv("get_server_info_v2", { serverId: s.id });
    const members = await inv("get_members", { serverId: s.id });
    const pk = (n) => { const m = members.find((x) => x.display_name === n); return "vk_" + m.public_key.bytes.map((b) => b.toString(16).padStart(2, "0")).join(""); };
    return { serverId: s.id, logServerId: i.server_id, alice: pk("Alice"), bob: pk("Bob") };
  }, name);
  const a = await info(A.page, "SelfTest " + stamp);
  const b = await info(B.page, "SelfTest " + stamp);
  const tries = async (fn) => { try { await fn(); return true; } catch { return false; } };
  const garbleAlice = () => call(B.page, "garble_set", { serverId: b.serverId, logServerId: b.logServerId, target: b.alice, mode: "bark", phrase: "", messages: 1, consensual: false });
  const makeInvite = () => call(B.page, "create_invite", { serverId: b.serverId, logServerId: b.logServerId, maxUses: 1, requiresApproval: false });

  await check("before: Bob (a plain member) cannot garble anyone or create an invite", async () => {
    if (await tries(garbleAlice)) throw new Error("Bob could garble without the power");
    if (await tries(makeInvite)) throw new Error("Bob could create an invite without the power");
  });
  await check("owner A grants Bob 'garble' and 'invite'; the list of power-holders shows it", async () => {
    await call(A.page, "grant_capability", { serverId: a.serverId, logServerId: a.logServerId, member: a.bob, capability: "garble" });
    await call(A.page, "grant_capability", { serverId: a.serverId, logServerId: a.logServerId, member: a.bob, capability: "invite" });
    await new Promise((r) => setTimeout(r, 2500));
    const holders = await call(A.page, "get_capabilities", { serverId: a.serverId });
    const bob = holders.find((h) => h.member === a.bob);
    if (!bob || !bob.capabilities.includes("garble") || !bob.capabilities.includes("invite")) throw new Error("holders list: " + JSON.stringify(holders));
  });
  await check("now Bob can create an invite, and his garble gets past the power check (the owner is protected)", async () => {
    await makeInvite();                                    // allowed only with the 'invite' power
    let msg = "";
    try { await garbleAlice(); } catch (e) { msg = String(e.message || e); }
    // Alice owns the server and can never be garbled. Reaching THAT rule (instead of
    // "not allowed") shows the 'garble' power is in force.
    if (!/owner cannot be garbled/.test(msg)) throw new Error("expected the owner protection, got: " + msg.slice(0, 120));
    return "invite created; garble stopped only by: owner cannot be garbled";
  });
  await check("owner A takes the powers back; Bob is refused again", async () => {
    await call(A.page, "revoke_capability", { serverId: a.serverId, logServerId: a.logServerId, member: a.bob, capability: "garble" });
    await call(A.page, "revoke_capability", { serverId: a.serverId, logServerId: a.logServerId, member: a.bob, capability: "invite" });
    await new Promise((r) => setTimeout(r, 2500));
    if (await tries(garbleAlice)) throw new Error("Bob could still garble after the power was taken");
    if (await tries(makeInvite)) throw new Error("Bob could still create an invite after the power was taken");
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Voice channels, signalling only: a voice channel can be created, both people join
// it, each sees the other in the roster, and leaving removes them. This does NOT open
// a microphone or speakers (that would capture the room on the machine running the
// test): the audio path itself is not covered here. Run after `core`.
async function voice() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    await c.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
    await c.page.locator(".channel-item", { hasText: "general" }).first().click().catch(() => {});
  }
  const call = (page, cmd, args) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  const sid = (page) => page.evaluate(async (nm) => (await window.__TAURI_INTERNALS__.invoke("get_saved_servers")).find((x) => x.name === nm).id, "SelfTest " + stamp);
  const aSid = await sid(A.page), bSid = await sid(B.page);
  const vname = "Voice" + stamp;
  let chA, chB;
  await check("A creates a voice channel; it appears for B", async () => {
    await call(A.page, "create_channel", { serverId: aSid, name: vname, channelType: "Voice", categoryId: null });
    await B.page.locator(".channel-item", { hasText: vname }).first().waitFor({ timeout: 30000 });
    const find = (page, s) => page.evaluate(async ([sv, nm]) => { const i = await window.__TAURI_INTERNALS__.invoke("get_server_info_v2", { serverId: sv }); return i.channels.find((c) => (c.base ?? c).name === nm)?.base?.id ?? i.channels.find((c) => (c.base ?? c).name === nm)?.id; }, [s, vname]);
    chA = await find(A.page, aSid); chB = await find(B.page, bSid);
    if (!chA || chA !== chB) throw new Error("channel ids differ: " + chA + " vs " + chB);
    await shot(B.page, "B-voice-channel");
  });
  await check("both join the voice channel; each side's roster lists both people", async () => {
    await call(A.page, "join_voice", { serverId: aSid, channelId: chA });
    await call(B.page, "join_voice", { serverId: bSid, channelId: chB });
    await new Promise((r) => setTimeout(r, 2500));
    for (const [name, page, s] of [["A", A.page, aSid], ["B", B.page, bSid]]) {
      const roster = await call(page, "get_voice_state", { serverId: s, channelId: chA });
      const names = roster.map((m) => m.display_name || m.displayName).sort();
      if (names.length !== 2) throw new Error(`${name} sees ${JSON.stringify(roster).slice(0, 160)}`);
    }
  });
  await check("B leaves: A's roster drops to one", async () => {
    await call(B.page, "leave_voice", { serverId: bSid, channelId: chB });
    await new Promise((r) => setTimeout(r, 2500));
    const roster = await call(A.page, "get_voice_state", { serverId: aSid, channelId: chA });
    if (roster.length !== 1) throw new Error("A still sees " + roster.length);
  });
  await call(A.page, "leave_voice", { serverId: aSid, channelId: chA }).catch(() => {});
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Phone-size layout: emulate a 390x844 phone screen on A and look for the things that
// break at that width (sideways scrolling, controls too small to tap, text cut off).
// This is a size emulation of the desktop app, not a phone: touch, keyboard and system
// bars are not simulated. Screenshots are kept for a person to look at. Run after `core`.
async function phone() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  if (await A.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(A);
  await A.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click().catch(() => {});
  const cdp = await A.ctx.newCDPSession(A.page);
  const set = (w, h) => cdp.send("Emulation.setDeviceMetricsOverride", { width: w, height: h, deviceScaleFactor: 2, mobile: true });
  const measure = () => A.page.evaluate(() => {
    const vw = window.innerWidth;
    const small = [];
    for (const el of document.querySelectorAll("button, [role=button], input, textarea, select, .channel-item, .server-icon")) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;                      // hidden
      if (Math.min(r.width, r.height) < 28) small.push((el.className || el.tagName).toString().slice(0, 30) + ` ${Math.round(r.width)}x${Math.round(r.height)}`);
    }
    const wide = [...document.querySelectorAll("body *")].filter((el) => { const r = el.getBoundingClientRect(); return r.right > vw + 2 && r.width > 0 && getComputedStyle(el).position !== "fixed"; }).slice(0, 5).map((el) => (el.className || el.tagName).toString().slice(0, 30));
    return { vw, scrollW: document.documentElement.scrollWidth, small: [...new Set(small)].slice(0, 8), wide };
  });
  const inView = async (loc, w, h) => {
    const b = await loc.first().boundingBox();
    return !!b && b.x >= -1 && b.y >= -1 && b.x + b.width <= w + 1 && b.y + b.height <= h + 1;
  };
  for (const [w, h, label] of [[390, 844, "phone"], [360, 640, "small-phone"]]) {
    await check(`${label} ${w}x${h}: conversation list fills the screen, then a chat with a working Back button`, async () => {
      await set(w, h);
      await A.page.evaluate(() => { document.documentElement.dataset.phoneView = "list"; });   // start from the list each time
      await new Promise((r) => setTimeout(r, 1500));
      // 1) The list is showing and the conversation is findable on screen.
      const item = A.page.locator(".compact-list .channel-item", { hasText: "general" }).first();
      await item.waitFor({ timeout: 15000 });
      if (!(await inView(item, w, h))) throw new Error("the conversation list does not fit on the screen");
      if (await A.page.locator(".chat-panel").first().isVisible()) throw new Error("the chat is showing next to the list (should be one pane at a time)");
      await shot(A.page, `phone-${w}-list`);
      // 2) Tapping it shows the chat, with the typing box on screen and a Back button.
      await item.click();
      await A.page.locator(".chat-panel").first().waitFor({ state: "visible", timeout: 10000 });
      const input = A.page.getByPlaceholder(/Type a message/);
      if (!(await inView(input, w, h))) throw new Error("the typing box is not on the screen");
      const ib = await input.boundingBox();
      if (!ib || ib.width < w * 0.6) throw new Error(`the typing box is only ${Math.round(ib?.width ?? 0)}px wide on a ${w}px screen`);
      const back = A.page.locator(".phone-back");
      if (!(await back.first().isVisible())) throw new Error("no Back button in the chat");
      const m = await measure();
      await shot(A.page, `phone-${w}-chat`);
      if (m.scrollW > m.vw + 2) throw new Error(`page is ${m.scrollW}px wide on a ${m.vw}px screen`);
      // 3) Back returns to the list.
      await back.first().click();
      await A.page.locator(".compact-list .channel-item", { hasText: "general" }).first().waitFor({ state: "visible", timeout: 10000 });
      return `tap targets under 28px: ${m.small.length ? m.small.join("; ") : "none"}`;
    });
  }
  await cdp.send("Emulation.clearDeviceMetricsOverride").catch(() => {});
  await A.browser.close().catch(() => {});
}

// The pop-up window must be big enough for everything in it: nothing cut off at the
// bottom or the sides, for a short message, a long one, and with the reply box open.
// Run after `direct`.
async function popupSize() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const first = `.server-icon[title="SelfTest ${stamp}"]`;
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  await A.page.locator(first).click();
  await A.page.locator(".channel-item", { hasText: "general" }).first().click();
  await B.page.locator(`.server-icon[title="Direct ${stamp}"]`).click();
  const geometry = (pop) => pop.evaluate(() => {
    const cards = [...document.querySelectorAll(".notify-popup-card")];
    const stack = document.querySelector(".notify-popup-stack");
    const bottoms = cards.map((c) => c.getBoundingClientRect().bottom);
    const rights = cards.map((c) => c.getBoundingClientRect().right);
    const clipped = cards.map((c) => c.scrollHeight - c.clientHeight);       // a card whose own content is cut off
    return {
      win: { w: window.innerWidth, h: window.innerHeight, dpr: window.devicePixelRatio },
      page: { scrollH: document.documentElement.scrollHeight, scrollW: document.documentElement.scrollWidth },
      stack: stack ? { h: Math.round(stack.getBoundingClientRect().height), top: Math.round(stack.getBoundingClientRect().top) } : null,
      cardBottom: Math.round(Math.max(0, ...bottoms)), cardRight: Math.round(Math.max(0, ...rights)),
      bodyMargin: getComputedStyle(document.body).margin,
      cardClip: Math.max(0, ...clipped),
    };
  });
  const verdict = (g, what) => {
    const problems = [];
    if (g.cardBottom + 1 > g.win.h) problems.push(`the card runs ${g.cardBottom - g.win.h}px below the window`);
    if (g.cardRight + 1 > g.win.w) problems.push(`the card runs ${g.cardRight - g.win.w}px past the window's right edge`);
    if (g.page.scrollH > g.win.h + 1) problems.push(`the page is ${g.page.scrollH}px tall in a ${g.win.h}px window`);
    if (g.cardClip > 1) problems.push(`the card's own content is cut off by ${g.cardClip}px`);
    if (problems.length) throw new Error(`${what}: ${problems.join("; ")}  [${JSON.stringify(g)}] size requests so far: ${reports.slice(-6).join(" ")}`);
    return `${what}: window ${g.win.w}x${g.win.h}, card ends at ${g.cardBottom} (margin ${g.bodyMargin})`;
  };
  let pop;
  const reports = [];   // every size the pop-up page asked its window to be
  await check("short message: the whole pop-up fits in its window", async () => {
    await sendText(A, "size test short " + stamp);
    pop = await pageMatching(B, /[?&]notify=1/, 30000);
    pop.on("request", (r) => { if (/notify_layout/.test(r.url())) reports.push(`${r.postData()}@${Date.now() % 100000}`); });
    await pop.locator(".notify-popup-card", { hasText: "size test short " + stamp }).first().waitFor({ timeout: 30000 });
    await new Promise((r) => setTimeout(r, 600));
    await shot(pop, "popup-short");
    return verdict(await geometry(pop), "short");
  });
  if (!pop) return;
  await check("long message: the whole pop-up fits in its window", async () => {
    await new Promise((r) => setTimeout(r, 7000));                                 // let the first card go
    await sendText(A, "size test long " + ("this is a much longer message that has to wrap onto several lines " .repeat(4)) + stamp);
    await pop.locator(".notify-popup-card", { hasText: "size test long" }).first().waitFor({ timeout: 30000 });
    await new Promise((r) => setTimeout(r, 600));
    await shot(pop, "popup-long");
    return verdict(await geometry(pop), "long");
  });
  await check("reply box open: the whole pop-up (with the box and its button) fits", async () => {
    await new Promise((r) => setTimeout(r, 7000));
    await sendText(A, "size test reply " + stamp);
    await pop.locator(".notify-popup-card", { hasText: "size test reply" }).first().waitFor({ timeout: 30000 });
    await pop.locator(".notify-popup-reply-btn").first().click();
    await pop.locator(".notify-popup-reply-input").first().waitFor({ timeout: 10000 });
    await new Promise((r) => setTimeout(r, 800));
    await shot(pop, "popup-reply-open");
    return verdict(await geometry(pop), "reply open");
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// A server running on UBUNTU (Linux), not on Windows: the Windows client claims it with
// the one-time setup token, invites the second client, and they chat. Needs the server
// started on the Linux side first; its address and token come in as UBU_ADDR / UBU_TOKEN
// (see docs/testing/feature-matrix.md). Run after `core` or on its own.
async function ubuntu() {
  const addr = process.env.UBU_ADDR, token = process.env.UBU_TOKEN;
  // UBU_LINK: the whole first-owner link (used for a server reached THROUGH THE RELAY);
  // UBU_NAME: the server's name as shown in the strip. Otherwise a direct link is built.
  const firstLink = process.env.UBU_LINK || (addr && token ? `farder://${addr}/setup:${token}` : null);
  if (!firstLink) throw new Error("set UBU_LINK, or UBU_ADDR (host:port) and UBU_TOKEN");
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    // A dialog left open by an earlier scenario would block every click.
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
  }
  if (!(await check("A (Windows) claims the Linux server with its setup token and becomes the owner", async () => {
    await A.page.getByTitle("Add Server").click();
    await A.page.getByText("Join a Server").click();
    await A.page.getByPlaceholder("farder://server/invite-code").fill(firstLink);
    await A.page.getByRole("button", { name: "Join", exact: true }).click();
    const confirm = A.page.getByText(/Join it\?/);
    if (await confirm.waitFor({ timeout: 6000 }).then(() => true, () => false)) await A.page.getByRole("button", { name: "Join", exact: true }).last().click();
    await A.page.locator(`.server-icon[title="${name}"]`).waitFor({ timeout: 90000 });
    await A.page.locator(`.server-icon[title="${name}"]`).click();
    await A.page.locator(".channel-item", { hasText: "general" }).first().waitFor({ timeout: 30000 });
    await skipTranslation(A);
    await shot(A.page, "A-ubuntu-owner");
  }))) return;
  let link;
  if (!(await check("A creates an invite on the Linux server", async () => { link = await makeInvite(A); return link.slice(0, 48); }))) return;
  if (!(await check("B (Windows) joins the Linux server from the invite", async () => {
    await B.page.getByTitle("Add Server").click();
    await B.page.getByText("Join a Server").click();
    await B.page.getByPlaceholder("farder://server/invite-code").fill(link);
    await B.page.getByRole("button", { name: "Join", exact: true }).click();
    const confirm = B.page.getByText(/Join it\?/);
    if (await confirm.waitFor({ timeout: 8000 }).then(() => true, () => false)) await B.page.getByRole("button", { name: "Join", exact: true }).last().click();
    await B.page.locator(`.server-icon[title="${name}"]`).waitFor({ timeout: 90000 });
    await B.page.locator(`.server-icon[title="${name}"]`).click();
    await B.page.locator(".channel-item", { hasText: "general" }).first().waitFor({ timeout: 30000 });
  }))) return;
  await check("chat both ways across Windows clients and the Linux server", async () => {
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await sendText(A, "hello from A to ubuntu " + stamp);
    await seesText(B, "hello from A to ubuntu " + stamp, 30000);
    await sendText(B, "hello from B to ubuntu " + stamp);
    await seesText(A, "hello from B to ubuntu " + stamp, 30000);
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// After the Linux server was backed up, wiped and restored (see the steps in
// docs/hosting-on-ubuntu.md): both clients must find the history, still be members, and
// be able to chat again, with no new invite. Run after `ubuntu` + the restore.
async function ubuntuRestored() {
  const name = "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    await c.page.locator(`.server-icon[title="${name}"]`).click();
  }
  await check("A: the history from before the wipe is back after the restore", async () => {
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await seesText(A, "hello from A to ubuntu " + stamp, 90000);
    await seesText(A, "hello from B to ubuntu " + stamp, 30000);
  });
  await check("B: the same history, and B is still a member (no new invite)", async () => {
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await seesText(B, "hello from A to ubuntu " + stamp, 90000);
  });
  // Right after the server comes back the clients are still reconnecting, so the first
  // send may fail. Try again like a person would, for up to a minute, and report how long it took.
  const sendUntilSeen = async (from, to, text) => {
    const t0 = Date.now();
    for (let i = 0; i < 12; i++) {
      const box = from.page.getByPlaceholder(/Type a message/);
      if (await box.isEnabled().catch(() => false)) {
        await box.fill(text); await box.press("Enter");
        if (await to.page.locator(".message-content", { hasText: text }).first().waitFor({ timeout: 5000 }).then(() => true, () => false)) return Math.round((Date.now() - t0) / 1000);
      } else await new Promise((r) => setTimeout(r, 5000));
    }
    throw new Error("could not send within a minute (the message box stayed " + ((await from.page.getByPlaceholder(/Type a message/).isEnabled().catch(() => false)) ? "enabled" : "disabled") + ")");
  };
  await check("both can chat on the restored server (clients reconnect on their own)", async () => {
    const a = await sendUntilSeen(A, B, "after the restore from A " + stamp);
    const b = await sendUntilSeen(B, A, "after the restore from B " + stamp);
    return `A's line seen after ${a}s, B's after ${b}s`;
  });
  await check("A is still the owner (the server settings button is there)", async () => {
    await A.page.getByTitle("Server Settings").waitFor({ timeout: 10000 });
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// A server that is DOWN must not lock the app: its history stays readable, a clear banner
// says what is happening, and the person can still switch to another server and chat there.
// Run with a server's host switched off (see docs/hosting-on-ubuntu.md); needs `core` + a
// saved server whose host is off (here "Ubuntu Server" after the restore test).
async function serverDown() {
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const down = process.env.UBU_NAME || "Ubuntu Server";
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
  }
  await check("the downed server shows a banner and its history is still readable", async () => {
    await A.page.locator(`.server-icon[title="${down}"]`).click();
    await A.page.locator(".reconnect-pill").waitFor({ timeout: 90000 });
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await A.page.locator(".message-content").first().waitFor({ timeout: 15000 });
    await shot(A.page, "A-server-down");
  });
  await check("while it is down, A can still switch to another server and chat there", async () => {
    await A.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click({ timeout: 8000 });
    await B.page.locator(`.server-icon[title="SelfTest ${stamp}"]`).click();
    await A.page.locator(".channel-item", { hasText: "general" }).first().click();
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await sendText(A, "chat while another server is down " + stamp);
    await seesText(B, "chat while another server is down " + stamp, 30000);
  });
  await check("the add-server button still works while a server is down", async () => {
    await A.page.getByTitle("Add Server").click({ timeout: 8000 });
    await A.page.getByText("Join a Server").waitFor({ timeout: 8000 });
    await A.page.locator(".modal-close").first().click();     // Escape does not close this dialog
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// The recovery owner, on a server where A (Windows) is the owner and B (Windows) a member
// (run `ubuntu` first; the server must be started with a SHORT --recovery-wait-secs).
// A makes the recovery key and writes the words down; B uses the words to take the server;
// A (the "thief" holding the everyday owner key) cannot stop it; after the wait B owns it.
async function recovery() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
    await c.page.locator(".channel-item", { hasText: "general" }).first().click();
  }
  const openRecovery = async (c) => {
    await c.page.getByTitle("Server Settings").click();
    await c.page.locator(".settings-tab", { hasText: "Recovery" }).click();
  };
  const closeDialog = async (c) => { await c.page.locator(".modal-close").first().click().catch(() => {}); };
  await check("A (the owner) is nudged that this server has no recovery key", async () => {
    await A.page.locator(".recovery-banner.nudge", { hasText: "no recovery key" }).waitFor({ timeout: 30000 });
    await shot(A.page, "A-recovery-nudge");
  });
  let words = "";
  if (!(await check("A (owner) makes a recovery key and is shown 24 words once", async () => {
    await openRecovery(A);
    await A.page.getByRole("button", { name: "Make a recovery key" }).waitFor({ timeout: 15000 });
    await A.page.getByRole("button", { name: "Make a recovery key" }).click();
    const box = A.page.getByLabel("Recovery words", { exact: true });
    await box.waitFor({ timeout: 30000 });
    words = await box.inputValue();
    const n = words.trim().split(/\s+/).length;
    if (n !== 24) throw new Error("expected 24 words, got " + n);
    await shot(A.page, "A-recovery-words");
    await A.page.getByLabel(/I wrote the 24 words down/).check();
    await A.page.getByRole("button", { name: "Hide the words" }).click();
    if (await A.page.getByLabel("Recovery words", { exact: true }).count()) throw new Error("the words are still on the screen");
    return "24 words shown once, then hidden";
  }))) return;
  await closeDialog(A);
  await check("the nudge goes away once the key exists", async () => {
    await A.page.locator(".recovery-banner.nudge").waitFor({ state: "detached", timeout: 30000 });
  });
  await check("B (a member) sees that a recovery key is set, but not the words", async () => {
    await openRecovery(B);
    await B.page.getByText(/A recovery key is set/).waitFor({ timeout: 20000 });
    if ((await B.page.locator("body").innerText()).includes(words.split(" ")[0] + " " + words.split(" ")[1] + " " + words.split(" ")[2])) throw new Error("B can see the words");
  });
  await check("the thief (A, with the everyday owner key) cannot use or guess the recovery key", async () => {
    // A tries to start a takeover with made-up words: refused, nothing changes.
    const bad = Array.from({ length: 24 }, () => "abandon").join(" ");
    const ids = await A.page.evaluate(async (nm) => {
      const inv = window.__TAURI_INTERNALS__.invoke;
      const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
      const info = await inv("get_server_info_v2", { serverId: s.id });
      const mem = await inv("get_members", { serverId: s.id });
      const hex = (m) => "vk_" + m.public_key.bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
      return { sid: s.id, log: info.server_id, alice: hex(mem.find((m) => m.display_name === "Alice")) };
    }, name);
    let refused = "";
    try { await A.page.evaluate(([a, l, p, o]) => window.__TAURI_INTERNALS__.invoke("recovery_claim", { serverId: a, logServerId: l, phrase: p, newOwner: o }), [ids.sid, ids.log, bad, ids.alice]); }
    catch (e) { refused = String(e.message || e); }
    if (!refused) throw new Error("the thief started a takeover with made-up words");
    // A VALID phrase (correct spelling and checksum) that is not this server's key must also be refused.
    const valid = "alpha deal scrub asthma idea logic bright thought alpha deal scrub asthma idea logic bright thought alpha deal scrub asthma idea logic bright truly";
    let refused2 = "";
    try { await A.page.evaluate(([a, l, p, o]) => window.__TAURI_INTERNALS__.invoke("recovery_claim", { serverId: a, logServerId: l, phrase: p, newOwner: o }), [ids.sid, ids.log, valid, ids.alice]); }
    catch (e) { refused2 = String(e.message || e); }
    if (!/not the recovery key of this server/.test(refused2)) throw new Error("a valid but wrong phrase was not refused as wrong-server: " + refused2);
    return "refused: " + refused.slice(0, 60) + " | valid-but-wrong: " + refused2.slice(0, 60);
  });
  await check("B rotates the recovery key with the words; the OLD words stop working, the NEW ones are shown once", async () => {
    const old = words;
    await B.page.getByLabel("The 24 recovery words").fill(old);
    await B.page.getByRole("button", { name: "Replace the recovery key" }).click();
    const box = B.page.getByLabel("Recovery words", { exact: true });
    await box.waitFor({ timeout: 30000 });
    const fresh = await box.inputValue();
    if (fresh.trim().split(/\s+/).length !== 24) throw new Error("expected 24 new words");
    if (fresh === old) throw new Error("the new words are the same as the old");
    await B.page.getByLabel(/I wrote the 24 words down/).check();
    await B.page.getByRole("button", { name: "Hide the words" }).click();
    // The OLD words are now refused as "not this server's recovery key".
    const res = await B.page.evaluate(async ([nm, p]) => {
      const inv = window.__TAURI_INTERNALS__.invoke;
      const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
      const info = await inv("get_server_info_v2", { serverId: s.id });
      const mem = await inv("get_members", { serverId: s.id });
      const hex = (m) => "vk_" + m.public_key.bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
      try { await inv("recovery_claim", { serverId: s.id, logServerId: info.server_id, phrase: p, newOwner: hex(mem.find((m) => m.display_name === "Bob")) }); return "accepted"; }
      catch (e) { return String(e); }
    }, [name, old]);
    if (!/not the recovery key of this server/.test(res)) throw new Error("the old words were not refused: " + res.slice(0, 100));
    words = fresh;
    return "old words refused after the rotation";
  });
  await check("B starts a takeover with the 24 words; both screens show it, with a countdown", async () => {
    for (const c of [A, B]) await c.page.evaluate(() => { window.__toasts = []; window.addEventListener("farder:toast", (e) => window.__toasts.push(e.detail.message)); });
    await B.page.getByLabel("The 24 recovery words").fill(words);
    await B.page.getByRole("button", { name: "Start the takeover" }).click();
    await B.page.locator(".modal-body").getByText(/Ownership of this server is moving to/).waitFor({ timeout: 30000 });
    await shot(B.page, "B-takeover-started");
    await closeDialog(B);
    await openRecovery(A);
    await A.page.locator(".modal-body").getByText(/Ownership of this server is moving to/).waitFor({ timeout: 30000 });
    await shot(A.page, "A-sees-takeover");
    await closeDialog(A);
    // Every member sees it on the main screen, without opening any settings.
    await A.page.locator(".recovery-banner", { hasText: "Ownership of this server is moving" }).waitFor({ timeout: 30000 });
    await B.page.locator(".recovery-banner", { hasText: "Ownership of this server is moving" }).waitFor({ timeout: 30000 });
    // ...and once as a pop-up on each screen. A pop-up fades after a few seconds, so each page recorded
    // the pop-up events from before the takeover started.
    for (const [c, label] of [[A, "A"], [B, "B"]]) {
      const heard = await c.page.evaluate(() => (window.__toasts || []).some((m) => /recovery key has started taking over/.test(m)));
      if (!heard) throw new Error("no takeover pop-up on " + label);
    }
    await shot(A.page, "A-banner");
  });
  await check("finishing early is not possible (the button waits), and A cannot cancel it", async () => {
    await closeDialog(A);
    await openRecovery(B);
    await B.page.getByLabel("The 24 recovery words").fill(words);
    if (await B.page.getByRole("button", { name: "Finish the takeover" }).isEnabled()) throw new Error("Finish is enabled during the wait");
    // The everyday owner key has no way to cancel: the Cancel button needs the words A does not have,
    // and a direct attempt with other words is refused.
    await closeDialog(B);
  });
  await check("after the waiting time B finishes it and B is the owner", async () => {
    await openRecovery(B);
    await B.page.getByLabel("The 24 recovery words").fill(words);
    await B.page.getByText(/can finish now/).waitFor({ timeout: 120000 });
    await B.page.getByRole("button", { name: "Finish the takeover" }).click();
    await B.page.getByText(/Done. The new owner is in charge/).waitFor({ timeout: 30000 });
    await closeDialog(B);
    // Owner-only tab appears for B, and is gone for A.
    await B.page.getByTitle("Server Settings").click();
    await B.page.locator(".settings-tab", { hasText: "Powers" }).waitFor({ timeout: 30000 });
    await closeDialog(B);
    await A.page.getByTitle("Server Settings").click();
    await new Promise((r) => setTimeout(r, 1500));
    if (await A.page.locator(".settings-tab", { hasText: "Powers" }).count()) throw new Error("A still sees the owner's Powers tab");
    await closeDialog(A);
    // And the banner is gone for everyone.
    // (the new owner is then nudged to make a fresh recovery key, since the old one was spent)
    await A.page.locator(".recovery-banner:not(.nudge)").waitFor({ state: "detached", timeout: 30000 });
    await B.page.locator(".recovery-banner:not(.nudge)").waitFor({ state: "detached", timeout: 30000 });
    await B.page.locator(".recovery-banner.nudge").waitFor({ timeout: 30000 });
  });
  await check("the old owner (still connected) can no longer do owner-only things; the new owner can", async () => {
    const act = (c, nm, cmd, extra) => c.page.evaluate(async ([name, command, more]) => {
      const inv = window.__TAURI_INTERNALS__.invoke;
      const s = (await inv("get_saved_servers")).find((x) => x.name === name);
      try { await inv(command, { serverId: s.id, ...more }); return "ok"; } catch (e) { return "refused: " + String(e).slice(0, 80); }
    }, [nm, cmd, extra]);
    const a = await act(A, name, "create_channel", { name: "owner-only-" + stamp, channelType: "Text", categoryId: null });
    if (a === "ok") throw new Error("the OLD owner could still create a channel");
    const b = await act(B, name, "create_channel", { name: "owner-only-" + stamp, channelType: "Text", categoryId: null });
    if (b !== "ok") throw new Error("the NEW owner could not create a channel: " + b);
    return `old owner: ${a}; new owner: ok`;
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// The host-side owner reset (`farder-server --reset-owner`): run after `ubuntu` and after the
// host stopped the server, reset the owner to B (a member) and started it again. B must own the
// server, A (the old owner) must not, and the old messages must still be there. Run it a second
// time after another restart to prove the answer is the same.
async function hostReset() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const pass = process.env.RESET_PASS || "1";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
  }
  await B.page.locator(".channel-item", { hasText: "general" }).first().click();
  await check("B still sees the old messages after the reset", async () => {
    await seesText(B, "hello from A to ubuntu " + stamp, 90000);
    await seesText(B, "hello from B to ubuntu " + stamp, 30000);
  });
  const act = (c, cmd, extra) => c.page.evaluate(async ([nm, command, more]) => {
    const inv = window.__TAURI_INTERNALS__.invoke;
    const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
    try { await inv(command, { serverId: s.id, ...more }); return "ok"; } catch (e) { return "refused: " + String(e).slice(0, 90); }
  }, [name, cmd, extra]);
  const chan = "reset" + pass + "-" + Date.now();
  await check("the NEW owner (B) can do owner-only things", async () => {
    let r = "";
    for (let i = 0; i < 12; i++) {
      r = await act(B, "create_channel", { name: chan, channelType: "Text", categoryId: null });
      if (r === "ok") return "ok after " + i + " retries";
      await new Promise((x) => setTimeout(x, 5000));
    }
    throw new Error("B could not act as owner: " + r);
  });
  await check("B has the owner's Powers tab", async () => {
    await B.page.getByTitle("Server Settings").click();
    await B.page.locator(".settings-tab", { hasText: "Powers" }).waitFor({ timeout: 30000 });
    await B.page.locator(".modal-close").first().click();
  });
  await check("the OLD owner (A) is refused owner-only things", async () => {
    const r = await act(A, "create_channel", { name: chan + "-a", channelType: "Text", categoryId: null });
    if (r === "ok") throw new Error("the old owner could still create a channel");
    // Before A rejoins, A is told WHAT HAPPENED and what to do, not just "not a member".
    if (pass === "1" && !/reset by its host.*fresh invite/.test(r)) throw new Error("not the plain reset message: " + r);
    return r;
  });
  if (pass === "1") {
    let link;
    await check("friends rejoin: B makes a fresh invite and A joins with it", async () => {
      await B.page.locator(".channel-item", { hasText: "general" }).first().click();
      link = await makeInvite(B);
      await A.page.getByTitle("Add Server").click();
      await joinWithLink(A, link);
    });
  }
  await check("A (rejoined or not) is NOT the owner: still refused owner-only things", async () => {
    const r = await act(A, "create_channel", { name: chan + "-a2", channelType: "Text", categoryId: null });
    if (r === "ok") throw new Error("the old owner could still create a channel");
    return r;
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Guest links from the app (docs/web-guest-join-design.md). Phase "make": A (the owner of the Ubuntu
// server, after `ubuntu`) opens the invite dialog, allows guest links and makes a guest link, which is
// written to GUEST_LINK_OUT for the browser test. Phase "seen": after the browser guest chatted, A and B
// must see the guest's message and the guest among the members.
async function guestLink() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const phase = process.env.GUEST_PHASE || "make";
  const out = process.env.GUEST_LINK_OUT;
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
    await c.page.locator(".channel-item", { hasText: "general" }).first().click();
  }
  if (phase === "make") {
    await check("A (owner) allows guest links and makes a guest link from the invite dialog", async () => {
      await A.page.getByTitle("Create Invite").click();
      await A.page.getByText(/Guest link \(for friends without the app/).click();
      await A.page.getByLabel(/Allow guest links on this server/).click();     // flips once the server confirms
      await A.page.getByLabel("Web name").waitFor({ timeout: 20000 });
      await A.page.getByLabel("Web name").fill(process.env.GUEST_ROUTE || "ubuntu");
      await A.page.getByLabel("Guest link duration").selectOption("3600");
      await A.page.getByRole("button", { name: "Make a guest link" }).click();
      const input = A.page.getByLabel("Guest link", { exact: true });
      await input.waitFor({ timeout: 30000 });
      const link = await input.inputValue();
      if (!/^https:\/\/[^/]+\/[A-Za-z0-9_-]+\/t#[A-Za-z0-9]+$/.test(link)) throw new Error("unexpected link " + link);
      if (out) fs.writeFileSync(out, link);
      await shot(A.page, "A-guest-link");
      await A.page.locator(".modal-close").first().click();
      return link;
    });
    await check("B (a normal member) is told the owner decides, or can make a link only if allowed", async () => {
      await B.page.getByTitle("Create Invite").click();
      await B.page.getByText(/Guest link \(for friends without the app/).click();
      await B.page.getByLabel("Web name").waitFor({ timeout: 20000 });      // on now, so B may make one too
      await B.page.locator(".modal-close").first().click();
    });
  } else {
    await check("the owner sees the browser guest's message and the guest in the member list", async () => {
      await seesText(A, "hello from a browser guest", 60000);
      await A.page.locator(".member-item", { hasText: "Webby" }).first().waitFor({ timeout: 30000 });
    });
    await check("the other member sees it too", async () => { await seesText(B, "hello from a browser guest", 60000); });
  }
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// The bulk-damage brake on the real stack: the OWNER (a stolen owner key is the case it exists for)
// can delete a handful of channels, then the server says "slow down" and the rest survive. Run after
// `ubuntu`, against a server started with the default budget (5 per 10 minutes).
async function brake() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
    await c.page.locator(".channel-item", { hasText: "general" }).first().click();
  }
  const call = (c, cmd, extra) => c.page.evaluate(async ([nm, command, more]) => {
    const inv = window.__TAURI_INTERNALS__.invoke;
    const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
    try { return { ok: await inv(command, { serverId: s.id, ...more }) }; } catch (e) { return { err: String(e) }; }
  }, [name, cmd, extra]);
  let ids = [];
  await check("the owner makes 7 throwaway channels", async () => {
    for (let i = 0; i < 7; i++) {
      const r = await call(A, "create_channel", { name: `brake-${stamp}-${i}`, channelType: "Text", categoryId: null });
      if (r.err) throw new Error(r.err);
    }
    for (let i = 0; i < 40 && ids.length < 7; i++) {
      const info = await A.page.evaluate(async (nm) => {
        const inv = window.__TAURI_INTERNALS__.invoke;
        const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
        return (await inv("get_server_info_v2", { serverId: s.id })).channels.map((c) => ({ id: (c.base || c).id, name: (c.base || c).name }));
      }, name);
      ids = info.filter((c) => c.name.startsWith(`brake-${stamp}-`)).map((c) => c.id);
      if (ids.length < 7) await new Promise((r) => setTimeout(r, 500));
    }
    if (ids.length !== 7) throw new Error("found " + ids.length + " of 7 channels");
  });
  await check("the first five deletions are allowed, the sixth gets a plain 'slow down'", async () => {
    for (let i = 0; i < 5; i++) {
      const r = await call(A, "delete_channel", { channelId: ids[i] });
      if (r.err) throw new Error(`deletion ${i + 1} refused: ${r.err}`);
    }
    const r = await call(A, "delete_channel", { channelId: ids[5] });
    if (!r.err || !/slow down/.test(r.err)) throw new Error("the sixth deletion was not stopped: " + JSON.stringify(r));
    return r.err.slice(0, 90);
  });
  await check("the sixth channel is still there on B's screen; ordinary work still goes through", async () => {
    await B.page.locator(".channel-item", { hasText: `brake-${stamp}-5` }).first().waitFor({ timeout: 20000 });
    const r = await call(A, "create_channel", { name: `after-brake-${stamp}`, channelType: "Text", categoryId: null });
    if (r.err) throw new Error(r.err);
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Reading a saved copy while the host is off (mesh step 2 + client failover). Run after `ubuntu`, in
// phases driven by ubuntu-suite.sh `failover` (FAILOVER_PHASE):
//   invite  A makes an invite for the copy machine (written to INVITE_OUT)
//   allow   A ticks the copy machine (REPLICA_KEY) in Server Settings > Backup copy; A and B type the
//           copy's address (FAILOVER_ADDR) as their backup address
//   down    (host stopped, copy running) B reaches the copy: banner, old messages, sending refused
//   back    (host started again) B switches back by itself and can send; A sees it
async function failover() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const phase = process.env.FAILOVER_PHASE || "invite";
  const backup = process.env.FAILOVER_ADDR || "";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
  }
  const setBackup = async (c) => {
    await c.page.getByTitle("Server Settings").click();
    await c.page.getByRole("button", { name: "Backup copy", exact: true }).click();
    await c.page.getByLabel("Backup addresses").fill(backup);
    await c.page.getByRole("button", { name: "Save", exact: true }).click();
    await c.page.getByText(/If the host does not answer, the app will try these/).waitFor({ timeout: 10000 });
  };
  if (phase === "invite") {
    await check("A makes an invite for the copy machine", async () => {
      const link = await makeInvite(A);
      fs.writeFileSync(process.env.INVITE_OUT, link);
      return link.slice(0, 48);
    });
  } else if (phase === "allow") {
    const key = process.env.REPLICA_KEY || "";
    await check("A (owner) sees the copy machine by its key in Backup copy and allows it", async () => {
      await A.page.getByTitle("Server Settings").click();
      await A.page.getByRole("button", { name: "Backup copy", exact: true }).click();
      const box = A.page.getByLabel(new RegExp(`\\(${key.slice(0, 11)}\\) keeps a copy`));
      await box.waitFor({ timeout: 20000 });
      await box.click();     // flips once the server confirms, so not .check()
      await A.page.waitForFunction((k) => document.querySelector(`label[data-key="${k}"] input`)?.checked === true, key, { timeout: 15000 });
      await shot(A.page, "A-backup-copy-allowed");
      await A.page.getByLabel("Backup addresses").fill(backup);
      await A.page.getByRole("button", { name: "Save", exact: true }).click();
      await A.page.getByText(/If the host does not answer, the app will try these/).waitFor({ timeout: 10000 });
      await A.page.locator(".modal-close").first().click();
    });
    await check("B (member) saves the copy's address as a backup address", async () => {
      await setBackup(B);
      await B.page.locator(".modal-close").first().click();
    });
    await check("A posts a picture while the host is up, and B sees it load", async () => {
      const { path, fileName } = await orangePng(stamp, "before-outage");
      for (const c of [A, B]) await c.page.locator(".channel-item", { hasText: "general" }).first().click();
      await dropAndSend(A, path, fileName, "picture before the outage " + stamp);
      await seesText(B, "picture before the outage " + stamp, 30000);
      await pictureLoads(B, "picture before the outage " + stamp);
    });
  } else if (phase === "down") {
    await check("with the host off, B ends up on the saved copy: the banner names the backup address", async () => {
      const t0 = Date.now();
      await B.page.locator(".recovery-banner", { hasText: "reading a saved copy" }).waitFor({ timeout: 120000 });
      const txt = await B.page.locator(".recovery-banner", { hasText: "reading a saved copy" }).innerText();
      if (!txt.includes(backup)) throw new Error("banner does not name " + backup + ": " + txt.slice(0, 120));
      await shot(B.page, "B-on-saved-copy");
      return `after ${Math.round((Date.now() - t0) / 1000)}s`;
    });
    // The screen alone would pass with NO server at all (the app keeps history on the device), so ask the
    // connection itself: it must be routed to the backup and the history must come FROM it.
    await check("B's connection is on the backup address and the old messages come from the copy", async () => {
      const r = await B.page.evaluate(async (nm) => {
        const inv = window.__TAURI_INTERNALS__.invoke;
        const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
        if (!s) return { err: "no saved server named " + nm };
        const route = await inv("get_connection_route", { serverId: s.id });
        const info = await inv("get_server_info_v2", { serverId: s.id });
        const general = info.channels.map((c) => c.base || c).find((c) => c.name === "general");
        const msgs = await inv("fetch_history", { serverId: s.id, channelId: general.id, beforeId: null, limit: 50 });
        return { route, contents: msgs.map((m) => m.content) };
      }, name);
      if (r.err) throw new Error(r.err);
      if (r.route !== backup) throw new Error(`route is ${r.route}, not ${backup}`);
      for (const t of ["hello from A to ubuntu " + stamp, "hello from B to ubuntu " + stamp])
        if (!r.contents.some((c) => c.includes(t))) throw new Error("the copy did not return: " + t);
      await B.page.locator(".channel-item", { hasText: "general" }).first().click();
      await seesText(B, "hello from A to ubuntu " + stamp, 30000);
      return `route ${r.route}, ${r.contents.length} messages from the copy`;
    });
    // The picture on screen could be the app's own memory of it, so fetch the file again over the
    // connection that is now on the copy, and compare the bytes with what A posted.
    await check("with the host off, the picture posted earlier comes from the copy, byte for byte", async () => {
      const { png } = await orangePng(stamp, "before-outage");
      const r = await B.page.evaluate(async ([nm, caption]) => {
        const inv = window.__TAURI_INTERNALS__.invoke;
        const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
        const route = await inv("get_connection_route", { serverId: s.id });
        const info = await inv("get_server_info_v2", { serverId: s.id });
        const general = info.channels.map((c) => c.base || c).find((c) => c.name === "general");
        const msgs = await inv("fetch_history", { serverId: s.id, channelId: general.id, beforeId: null, limit: 50 });
        const m = msgs.find((x) => (x.content || "").includes(caption));
        if (!m) return { err: "the copy has no message: " + caption };
        const a = (m.attachments || [])[0];
        if (!a) return { err: "the message came back without its attachment" };
        const d = await inv("download_file", { serverId: s.id, fileId: a.file_id });
        return { route, name: a.name, url: d.data_url || "" };
      }, [name, "picture before the outage " + stamp]);
      if (r.err) throw new Error(r.err);
      if (r.route !== backup) throw new Error(`route is ${r.route}, not ${backup}`);
      const got = Buffer.from(r.url.replace(/^data:[^,]*,/, ""), "base64");
      if (!got.equals(png)) throw new Error(`the copy served ${got.length} bytes that do not match the ${png.length} posted`);
      await B.page.locator(".channel-item", { hasText: "general" }).first().click();
      await pictureLoads(B, "picture before the outage " + stamp);
      await shot(B.page, "B-picture-from-copy");
      return `${r.name}: ${got.length} bytes, identical, via ${r.route}`;
    });
    await check("the server keeps its own name on the copy (not renamed to the default)", async () => {
      await B.page.locator(`.server-icon[title="${name}"]`).waitFor({ timeout: 5000 });
    });
    await check("B's send on the copy is refused with the plain 'saved copy' reason", async () => {
      await sendText(B, "sent while on the copy " + stamp);
      await B.page.locator(".error-text", { hasText: /saved copy/ }).waitFor({ timeout: 20000 });
      await shot(B.page, "B-send-refused");
      return (await B.page.locator(".error-text", { hasText: /saved copy/ }).first().innerText()).slice(0, 90);
    });
  } else {
    await check("with the host back, B leaves the copy by itself (the banner goes away)", async () => {
      const t0 = Date.now();
      await B.page.locator(".recovery-banner", { hasText: "reading a saved copy" }).waitFor({ state: "detached", timeout: 120000 });
      return `after ${Math.round((Date.now() - t0) / 1000)}s`;
    });
    await check("B can send on the host again and A sees it", async () => {
      const text = "back on the host " + stamp;
      for (let i = 0; i < 12; i++) {
        const box = B.page.getByPlaceholder(/Type a message/);
        if (await box.isEnabled().catch(() => false)) {
          await box.fill(text); await box.press("Enter");
          if (await A.page.locator(".message-content", { hasText: text }).first().waitFor({ timeout: 5000 }).then(() => true, () => false)) return;
        } else await new Promise((r) => setTimeout(r, 5000));
      }
      throw new Error("A never saw B's message");
    });
  }
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Message density (run after `ubuntu`). B posts a short burst, a long line and a picture; A switches
// every mode through Settings and the layout is MEASURED: one row per message in the dense modes, the
// IRC brackets and the tiny colon really drawn, nothing running off the edge, text readable in two
// themes, and the choice still set after a reload.
async function density() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
    await c.page.locator(".channel-item", { hasText: "general" }).first().click();
  }
  const tag = "dens" + stamp;
  const burst = [1, 2, 3].map((i) => `${tag} burst ${i}`);
  const long = `${tag} long ` + "a fairly long sentence that has to wrap instead of running off the edge ".repeat(30);  // wraps even in a 2500px-wide window
  await check("B posts a burst of three and one long message; A posts a picture", async () => {
    for (const t of [...burst, long]) await sendText(B, t);
    const { path, fileName } = await orangePng(stamp, "density");
    await dropAndSend(A, path, fileName, `${tag} picture`);
    await seesText(A, `${tag} picture`, 30000);
    await pictureLoads(A, `${tag} picture`);
  });
  const openDensity = async () => {
    await A.page.getByRole("button", { name: "Your settings" }).click();
    await A.page.locator(".settings-nav-item", { hasText: "Message density" }).click();
  };
  const choose = async (label) => {
    await openDensity();
    await A.page.locator(".settings-option", { hasText: label }).first().click();
    await A.page.locator(".modal-close").first().click();
    await new Promise((r) => setTimeout(r, 400));
  };
  const measure = () => A.page.evaluate((tag) => {
    const lum = (rgb) => { const m = rgb.match(/[\d.]+/g).map(Number); const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }; return 0.2126 * f(m[0]) + 0.7152 * f(m[1]) + 0.0722 * f(m[2]); };
    const bgOf = (el) => { for (let e = el; e; e = e.parentElement) { if (getComputedStyle(e).backgroundImage !== "none") return null; const c = getComputedStyle(e).backgroundColor; const m = c.match(/[\d.]+/g); if (m && (m.length < 4 || Number(m[3]) > 0.5)) return c; } return "rgb(255, 255, 255)"; };
    const contrast = (el) => { const bg = bgOf(el); if (bg === null) return null; const a = lum(getComputedStyle(el).color), b = lum(bg); return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05); };
    const list = document.querySelector(".message-list");
    const lr = list.getBoundingClientRect();
    const rows = [...document.querySelectorAll(".message")].filter((m) => (m.querySelector(".message-content")?.innerText || "").includes(tag));
    const vis = (el) => !!el && getComputedStyle(el).display !== "none" && el.getBoundingClientRect().width > 0;
    return {
      mode: document.documentElement.dataset.density,
      rows: rows.map((m) => {
        const content = m.querySelector(".message-content");
        const author = m.querySelector(".message-author");
        const av = m.querySelector(".message-avatar");
        const ts = m.querySelector(".message-timestamp");
        const att = m.querySelector(".message-attachments");
        const cs = getComputedStyle(content);
        const lh = parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.3;
        const r = m.getBoundingClientRect();
        return {
          text: content.innerText.slice(0, 40),
          h: r.height, lh, right: r.right, contentRight: content.getBoundingClientRect().right,
          avatar: vis(av) ? av.getBoundingClientRect().width : 0,
          author: vis(author),
          authorBefore: author ? getComputedStyle(author, "::before").content : "",
          authorAfter: author ? getComputedStyle(author, "::after").content : "",
          tsBefore: ts ? getComputedStyle(ts, "::before").content : "",
          tsOpacity: ts ? Number(getComputedStyle(ts).opacity) : 0,
          tsText: ts ? ts.innerText : "",
          mono: /mono|consolas/i.test(getComputedStyle(m).fontFamily),
          attBelowAuthor: att && author ? att.getBoundingClientRect().top >= author.getBoundingClientRect().bottom - 1 : null,
          contrast: contrast(content),
        };
      }),
      listRight: lr.right,
      sideways: list.scrollWidth > list.clientWidth + 2,
    };
  }, tag);
  await check("A reacts to burst 1 (so every mode has a reaction to show)", async () => {
    const row = A.page.locator(".message", { hasText: burst[0] }).first();
    await row.hover();
    // The add button lives in the hover-only bar (opacity 0 until pointed at), so Playwright
    // calls it invisible even while hovered; a person pointing at the message sees it.
    await row.locator(".reaction-add-btn").click({ force: true });
    // The picker must be wholly inside the window and laid out as a grid, not one tall column.
    const pk = await A.page.evaluate(() => { const p = document.querySelector(".reaction-picker"); const r = p.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, vw: innerWidth, vh: innerHeight }; });
    if (pk.y < 0 || pk.x < 0 || pk.x + pk.w > pk.vw || pk.y + pk.h > pk.vh) throw new Error("the picker runs outside the window: " + JSON.stringify(pk));
    if (pk.w < 200) throw new Error(`the picker is squeezed to ${Math.round(pk.w)}px wide`);
    await A.page.locator(".reaction-picker .reaction-picker-btn").first().click();
    await row.locator(".reaction-count").first().waitFor({ timeout: 15000 });
  });
  const results = {};
  for (const [label, mode] of [["Normal", "normal"], ["Roomy", "roomy"], ["Compact", "compact"], ["IRC", "irc"], ["Tiny", "tiny"]]) {
    await check(`density ${label}: chosen in Settings, laid out as described, nothing off the edge`, async () => {
      await choose(label);
      const m = await measure();
      results[mode] = m;
      await shot(A.page, "density-" + mode);
      if (m.mode !== mode) throw new Error(`page says ${m.mode}, not ${mode}`);
      if (m.rows.length < 5) throw new Error(`only ${m.rows.length} test messages on screen`);
      if (m.sideways) throw new Error("the message list scrolls sideways");
      for (const r of m.rows) if (r.contentRight > m.listRight + 1) throw new Error(`"${r.text}" runs past the edge`);
      const short = m.rows.filter((r) => r.text.includes("burst"));
      // Burst 1 carries a reaction (its own line), so the one-line yardstick is bursts 2 and 3.
      const plain = short.filter((r) => !r.text.includes("burst 1"));
      const longRow = m.rows.find((r) => r.text.includes(" long "));
      if (!(longRow.h > Math.min(...plain.map((r) => r.h)) * 1.5)) throw new Error("the long message did not wrap onto more lines");
      const named = short.filter((r) => r.author).length;
      const dense = ["compact", "irc", "tiny"].includes(mode);
      if (dense) {
        for (const r of plain) if (r.h > r.lh * 2.2) throw new Error(`"${r.text}" is ${Math.round(r.h)}px tall, not one line (${Math.round(r.lh)}px)`);
        if (named !== 3) throw new Error(`a burst of three shows the name ${named} times, want 3`);
        const pic = m.rows.find((r) => r.text.includes("picture"));
        if (pic.attBelowAuthor === false) throw new Error("the picture is not on its own line under the name");
      } else if (named > 1) throw new Error(`a burst of three shows the name ${named} times, want at most 1 (folded under one name)`);
      if (mode === "compact") {
        const normalAv = results.normal?.rows.find((r) => r.avatar > 0)?.avatar || 32;
        if (!(short[0].avatar > 0 && short[0].avatar < normalAv * 0.8)) throw new Error(`picture is ${short[0].avatar}px, not smaller than Normal's ${normalAv}px`);
        if (short[0].tsOpacity !== 0) throw new Error("the time shows without pointing at the message");
      }
      if (mode === "irc") {
        const r = short[0];
        if (r.avatar !== 0) throw new Error("IRC shows pictures");
        if (!r.mono) throw new Error("IRC is not in a monospace font");
        if (r.authorBefore !== '"<"' || r.authorAfter !== '">"' || r.tsBefore !== '"["') throw new Error(`not [time] <name>: ${r.tsBefore} ${r.authorBefore} ${r.authorAfter}`);
        if (!/^\d\d:\d\d$/.test(r.tsText)) throw new Error("IRC clock is not hh:mm: " + r.tsText);
      }
      if (mode === "tiny") {
        const r = short[0];
        if (r.avatar !== 0) throw new Error("tiny shows pictures");
        if (r.authorAfter !== '":"') throw new Error("tiny is not name: text");
        if (r.tsOpacity !== 0) throw new Error("tiny shows a clock");
      }
      const low = m.rows.map((r) => r.contrast).filter((c) => c !== null && c < 3);
      if (low.length) throw new Error("text hard to read, contrast " + low[0].toFixed(1));
      return `rows ${short.map((r) => Math.round(r.h)).join("/")}px (1st has a reaction), long ${Math.round(longRow.h)}px` + (short[0].avatar ? `, picture ${Math.round(short[0].avatar)}px` : "");
    });
  }
  for (const [label] of [["Compact"], ["IRC"], ["Tiny"]]) {
    await check(`density ${label}: the reaction shows, and right-click > Reply works`, async () => {
      await choose(label);
      const r = await A.page.evaluate((t) => {
        const row = [...document.querySelectorAll(".message")].find((m) => (m.querySelector(".message-content")?.innerText || "").includes(t));
        const c = row?.querySelector(".reaction-count");
        const list = document.querySelector(".message-list").getBoundingClientRect();
        if (!c) return { err: "no reaction on the row" };
        const b = c.getBoundingClientRect();
        return { w: b.width, inside: b.left >= list.left - 1 && b.right <= list.right + 1, visible: getComputedStyle(c).visibility !== "hidden" && b.width > 0 };
      }, burst[0]);
      if (r.err) throw new Error(r.err);
      if (!r.visible || !r.inside) throw new Error("the reaction is hidden or outside the list: " + JSON.stringify(r));
      const row2 = A.page.locator(".message", { hasText: burst[1] }).first();
      await row2.click({ button: "right" });
      const menu = A.page.locator(".context-menu").first();
      await menu.waitFor({ timeout: 5000 });
      const mb = await menu.boundingBox();
      const vw = await A.page.evaluate(() => [innerWidth, innerHeight]);
      if (mb.x + mb.width > vw[0] + 1 || mb.y + mb.height > vw[1] + 1) throw new Error("the menu runs off the window");
      await menu.locator(".context-menu-item", { hasText: "Reply" }).first().click();
      const bar = A.page.locator(".reply-preview");
      await bar.waitFor({ timeout: 5000 });
      const txt = await bar.innerText();
      await A.page.locator(".reply-cancel").click();
      if (!txt.includes("burst 2")) throw new Error("the reply bar quotes: " + txt.slice(0, 60));
      return "reaction visible, menu inside the window, reply bar quotes burst 2";
    });
  }
  await check("Roomy gives each message more room than Normal", async () => {
    const h = (m) => m.rows.filter((r) => r.text.includes("burst")).reduce((a, r) => a + r.h, 0);
    if (!results.roomy || !results.normal) throw new Error("a mode failed above");
    if (!(h(results.roomy) > h(results.normal))) throw new Error(`roomy ${h(results.roomy)}px vs normal ${h(results.normal)}px`);
    return `${Math.round(h(results.roomy))}px vs ${Math.round(h(results.normal))}px for the burst`;
  });
  const original = await A.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("get_active_theme").then((t) => t.id));
  const setTheme = (id) => A.page.evaluate(async (id) => {
    const css = await window.__TAURI_INTERNALS__.invoke("load_theme_css", { id });
    document.getElementById("active-theme").textContent = css;
    document.documentElement.dataset.theme = id;
  }, id);
  try {
    for (const theme of ["velvet", "candy"]) {
      await check(`theme ${theme}: every density mode stays readable and inside the window`, async () => {
        await setTheme(theme);
        const worst = [];
        for (const label of ["Roomy", "Compact", "IRC", "Tiny", "Normal"]) {
          await choose(label);
          const m = await measure();
          await shot(A.page, `density-${theme}-${m.mode}`);
          if (m.sideways) throw new Error(`${label}: the list scrolls sideways`);
          const cs = m.rows.map((r) => r.contrast).filter((c) => c !== null);
          if (cs.length && Math.min(...cs) < 3) throw new Error(`${label}: contrast ${Math.min(...cs).toFixed(1)}`);
          worst.push(cs.length ? Math.min(...cs).toFixed(1) : "?");
        }
        return "lowest contrast per mode " + worst.join(", ");
      });
    }
  } finally {
    await setTheme(original);
  }
  await check("the chosen mode is still set after the app reloads", async () => {
    await choose("Compact");
    await A.page.reload();
    if (await A.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 8000 }).then(() => true, () => false)) await unlock(A);
    await A.page.waitForFunction(() => document.documentElement.dataset.density === "compact", null, { timeout: 15000 });
    await openDensity();
    const sel = await A.page.locator(".settings-option.selected").innerText();
    await A.page.locator(".modal-close").first().click();
    if (!sel.includes("Compact")) throw new Error("Settings shows " + sel.split("\n")[0]);
  });
  await choose("Normal").catch(() => {});
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Small-window layout (run after `ubuntu`): chosen in Settings, the window shrinks and only the
// conversation list + one chat remain; channels and DMs open there, unread shows, the server switcher,
// Settings gear and sending work, the window cannot be dragged below the minimum and nothing important
// is cut off at that size, Full window brings everything back, and a reload starts small again.
async function smallwin() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
    await c.page.locator(".channel-item", { hasText: "general" }).first().click();
  }
  const second = `side-${stamp}`.toLowerCase();
  const tag = "sw" + stamp;
  const win = () => A.page.evaluate(() => ({ w: window.innerWidth, h: window.innerHeight, layout: document.documentElement.dataset.layout }));
  const visible = (sel) => A.page.evaluate((sel) => { const e = document.querySelector(sel); if (!e) return false; const r = e.getBoundingClientRect(); return getComputedStyle(e).display !== "none" && r.width > 0 && r.height > 0; }, sel);
  // Resize A's window from OUTSIDE, the way a person drags its edge: the app (rightly) does not let its own
  // page resize it. A is the first process start-pair.ps1 recorded. Sizes are in screen pixels.
  const resize = async (w, h) => {
    const { execFileSync } = await import("node:child_process");
    const pathMod = await import("node:path");
    const pid = fs.readFileSync(pathMod.join(pathMod.dirname(process.env.STATE || "selftest-state.json"), "pids.txt"), "utf8").trim().split(/\s+/)[0];
    const ps = `Add-Type -Name W -Namespace U -MemberDefinition '[DllImport("user32.dll")] public static extern bool MoveWindow(System.IntPtr h, int x, int y, int w, int g, bool r);'; $p = Get-Process -Id ${pid}; [U.W]::MoveWindow($p.MainWindowHandle, 40, 40, ${w}, ${h}, $true) | Out-Null`;
    execFileSync("powershell.exe", ["-NoProfile", "-Command", ps]);
  };
  // The smallest size Windows lets a person DRAG the window to (WM_GETMINMAXINFO's ptMinTrackSize, screen
  // pixels, frame included). A program moving the window is not held to it, so that is what we ask for.
  const minTrack = async () => {
    const { execFileSync } = await import("node:child_process");
    const pathMod = await import("node:path");
    const pid = fs.readFileSync(pathMod.join(pathMod.dirname(process.env.STATE || "selftest-state.json"), "pids.txt"), "utf8").trim().split(/\s+/)[0];
    const cs = 'using System; using System.Runtime.InteropServices; public static class M { [StructLayout(LayoutKind.Sequential)] public struct P { public int x; public int y; } [StructLayout(LayoutKind.Sequential)] public struct I { public P a; public P b; public P c; public P minT; public P maxT; } [DllImport("user32.dll")] static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, ref I l); public static string Get(IntPtr h) { I i = new I(); SendMessage(h, 0x24, IntPtr.Zero, ref i); return i.minT.x + "x" + i.minT.y; } }';
    const ps = `Add-Type -TypeDefinition '${cs}'; [M]::Get((Get-Process -Id ${pid}).MainWindowHandle)`;
    const out = execFileSync("powershell.exe", ["-NoProfile", "-Command", ps]).toString().trim();
    const [w, h] = out.split("x").map(Number);
    return { w, h };
  };
  const settle = () => new Promise((r) => setTimeout(r, 1200));
  // A second channel, and B's direct message to A, so the list has both kinds and something to be unread.
  await check("setup: a second channel, so there is somewhere to be unread", async () => {
    await A.page.evaluate(async ([nm, ch]) => {
      const inv = window.__TAURI_INTERNALS__.invoke;
      const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
      await inv("create_channel", { serverId: s.id, name: ch, channelType: "Text", categoryId: null });
    }, [name, second]);
    await A.page.locator(".channel-item", { hasText: second }).first().waitFor({ timeout: 20000 });
    await B.page.locator(".channel-item", { hasText: second }).first().waitFor({ timeout: 20000 });
  });
  await check("Settings > Window layout > Small window: the window shrinks to the small layout", async () => {
    await A.page.getByRole("button", { name: "Your settings" }).click();
    await A.page.locator(".settings-nav-item", { hasText: "Window layout" }).click();
    await A.page.locator(".settings-option", { hasText: "Small window" }).first().click();
    await A.page.locator(".modal-dialog:visible .modal-close").first().click().catch(() => {});
    await settle();
    const w = await win();
    await shot(A.page, "small-window");
    if (w.layout !== "small") throw new Error("layout is " + w.layout);
    if (w.w > 700 || w.h > 500) throw new Error(`window is still ${w.w}x${w.h}`);
    for (const sel of [".compact-list", ".member-sidebar", ".server-strip"]) {
      const v = await visible(sel);
      if (sel === ".compact-list" ? !v : v) throw new Error(`${sel} ${v ? "is" : "is not"} showing`);
    }
    const list = await A.page.locator(".compact-list").innerText();
    for (const t of [name, "CHANNELS", "general", second, "Full window"]) if (!list.includes(t)) throw new Error("the list has no " + t);
    if ((await A.page.locator(".modal-dialog").count()) > 0) throw new Error("a dialog is still open (hidden?) after choosing the small window");
    const fit = await A.page.evaluate(() => {
      const box = document.querySelector(".message-input-textwrap textarea")?.getBoundingClientRect();
      const send = [...document.querySelectorAll(".message-input-row button")].find((b) => b.innerText.trim() === "Send")?.getBoundingClientRect();
      return { box: box ? Math.round(box.width) : 0, sendRight: send ? Math.round(send.right) : 99999, w: innerWidth };
    });
    if (fit.box < 180) throw new Error(`the message box is only ${fit.box}px wide`);
    if (fit.sendRight > fit.w) throw new Error(`Send is cut off (${fit.sendRight}px of a ${fit.w}px window)`);
    return `${w.w}x${w.h}, message box ${fit.box}px, Send inside`;
  });
  await check("click a channel in the list: it opens in the chat", async () => {
    await A.page.locator(".compact-list .channel-item", { hasText: "general" }).first().click();
    await seesText(A, "hello from B to ubuntu " + stamp, 15000);
  });
  await check("a message in another channel makes its row unread", async () => {
    await B.page.locator(".channel-item", { hasText: second }).first().click();
    await sendText(B, `${tag} over here`);
    await A.page.locator(".compact-list .channel-item.unread", { hasText: second }).waitFor({ timeout: 20000 });
    await shot(A.page, "small-window-unread");
    await A.page.locator(".compact-list .channel-item", { hasText: second }).first().click();
    await seesText(A, `${tag} over here`, 15000);
  });
  await check("click the server name: the server list and + Add server appear; Add server opens its dialog", async () => {
    await A.page.locator(".compact-list .server-name").click();
    await A.page.locator(".compact-list .channel-category", { hasText: "SERVERS" }).waitFor({ timeout: 5000 });
    await A.page.locator(".compact-list .channel-item", { hasText: "+ Add server" }).click();
    await A.page.locator(".modal-dialog:visible").first().waitFor({ timeout: 5000 });
    await shot(A.page, "small-window-add-server");
    await A.page.locator(".modal-dialog:visible .modal-close").first().click();
  });
  await check("the gear opens Settings; typing and sending works from the small window", async () => {
    await A.page.locator(".compact-list .server-invite-btn[title='Settings']").click();
    await A.page.getByText(/Your Settings/).first().waitFor({ timeout: 5000 });
    await A.page.locator(".modal-dialog:visible .modal-close").first().click();
    await A.page.locator(".compact-list .channel-item", { hasText: "general" }).first().click();
    await sendText(A, `${tag} from the small window`);
    await B.page.locator(".channel-item", { hasText: "general" }).first().click();
    await seesText(B, `${tag} from the small window`, 20000);
  });
  await check("the window cannot be made smaller than about 380x280, and at that size the chat header and message box fit", async () => {
    const min = await minTrack();
    const dpr = await A.page.evaluate(() => window.devicePixelRatio);
    // Allow for the frame and title bar: the INSIDE must still be about 380x280 logical.
    if (!(min.w / dpr >= 380 && min.h / dpr >= 280)) throw new Error(`Windows lets the window be dragged down to ${min.w}x${min.h} screen px (scale ${dpr})`);
    await resize(min.w, min.h);
    await settle();
    const w = await win();
    await shot(A.page, "small-window-minimum");
    const cut = await A.page.evaluate(() => {
      const bad = [];
      for (const sel of [".channel-header", ".message-input-textwrap textarea"]) {
        const e = document.querySelector(sel);
        if (!e) { bad.push(sel + " missing"); continue; }
        const r = e.getBoundingClientRect();
        if (r.bottom > window.innerHeight + 1 || r.right > window.innerWidth + 1 || r.width < 40 || r.height < 12) bad.push(`${sel} ${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}`);
      }
      return bad;
    });
    if (cut.length) throw new Error("cut off at the minimum: " + cut.join("; "));
    return `drag minimum ${min.w}x${min.h} screen px, inside ${w.w}x${w.h}`;
  });
  await check("Full window brings the window and everything back", async () => {
    if (await A.page.locator(".phone-back").isVisible().catch(() => false)) await A.page.locator(".phone-back").click();
    await A.page.locator(".compact-list button", { hasText: "Full window" }).click();
    await settle();
    const w = await win();
    if (w.layout !== "full") throw new Error("layout is " + w.layout);
    if (w.w < 1000 || w.h < 650) throw new Error(`window only grew to ${w.w}x${w.h}`);
    if (!(await visible(".member-sidebar"))) throw new Error("the member list did not come back");
    if (!(await visible(".server-strip"))) throw new Error("the server column did not come back");
    return `${w.w}x${w.h}`;
  });
  await check("a reload while in the small layout starts small again", async () => {
    await A.page.evaluate(() => localStorage.setItem("farder.layout", "small"));
    await A.page.reload();
    if (await A.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 8000 }).then(() => true, () => false)) await unlock(A);
    await A.page.waitForFunction(() => document.documentElement.dataset.layout === "small", null, { timeout: 15000 });
    await settle();
    const w = await win();
    if (w.w > 700) throw new Error(`layout small but the window is ${w.w}x${w.h}`);
    return `${w.w}x${w.h}`;
  });
  // Back to the full window for whatever runs next.
  await A.page.locator(".compact-list button", { hasText: "Full window" }).click().catch(() => {});
  await settle();
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Autoplay (run after `ubuntu`; needs the internet). In the chat only allowlisted providers get a video
// card (the relay never fetches arbitrary hosts, so a direct .mp4 link on any other site shows NO card),
// and of those only a tweet's video plays inline. So a public tweet with a short (9 s, 3 MB) video is used.
// A is the viewer, B the sender.
async function autoplay() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const VIDEO = process.env.AUTOPLAY_VIDEO || "https://x.com/elonmusk/status/1585341984679469056";
  const YT = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-dialog:visible .modal-close").count()) > 0; i++) await c.page.locator(".modal-dialog:visible .modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
    await c.page.locator(".channel-item", { hasText: "general" }).first().click();
  }
  const tag = "ap" + stamp;
  // The video card inside the message with `label`: is there a poster, a player, and what is it doing?
  const card = (label) => A.page.evaluate((label) => {
    const m = [...document.querySelectorAll(".message")].find((x) => (x.querySelector(".message-content")?.innerText || "").includes(label));
    if (!m) return { msg: false };
    const slot = m.querySelector(".media-slot");
    const v = slot?.querySelector("video.mp-media");
    const wrap = v?.closest(".mp-docked, .mp-float, .mp-mini");
    return {
      msg: true, slot: !!slot, poster: !!slot?.querySelector(".media-slot-poster"), iframe: !!slot?.querySelector("iframe"),
      video: !!v, paused: v ? v.paused : null, muted: v ? v.muted : null, t: v ? v.currentTime : 0,
      where: wrap ? wrap.className.split(" ")[0] : null, floating: document.querySelectorAll(".mp-float").length,
    };
  }, label);
  const until = async (label, pred, ms, what) => {
    const t0 = Date.now(); let last;
    while (Date.now() - t0 < ms) { last = await card(label); if (pred(last)) return last; await new Promise((r) => setTimeout(r, 1000)); }
    throw new Error(`${what}: ${JSON.stringify(last)}`);
  };
  const post = async (c, label, extra) => { await sendText(c, `${label} ${extra}`); await seesText(A, label, 30000); };
  const openSettings = async (section) => {
    await A.page.getByRole("button", { name: "Your settings" }).click();
    await A.page.locator(".settings-nav-item", { hasText: section }).click();
  };
  const closeDialogs = async () => { for (let i = 0; i < 3 && (await A.page.locator(".modal-dialog:visible .modal-close").count()) > 0; i++) await A.page.locator(".modal-dialog:visible .modal-close").first().click(); };
  try {
    await check("setup: enough messages that the video can be scrolled out of view", async () => {
      for (let i = 0; i < 14; i++) await sendText(B, `${tag} filler ${i}`);
      await seesText(A, `${tag} filler 13`, 30000);
    });
    await check("off by default: B's video link shows a poster with Play, nothing starts", async () => {
      await post(B, `${tag} first`, VIDEO);
      const r = await until(`${tag} first`, (x) => x.slot, 45000, "no video card under the link (preview through the relay)");
      await new Promise((r) => setTimeout(r, 6000));
      const after = await card(`${tag} first`);
      if (after.video) throw new Error("a player opened by itself: " + JSON.stringify(after));
      if (!after.poster) throw new Error("no Play poster: " + JSON.stringify(after));
      return "poster shown, no player";
    });
    await check("right-click Bob > Autoplay their media: autoplay turns on and Bob is on the list in Settings", async () => {
      await A.page.locator(".member-item", { hasText: "Bob" }).first().click({ button: "right" });
      await A.page.getByText("Autoplay their media", { exact: true }).click();
      await openSettings("Autoplay");
      const panel = A.page.locator(".settings-content");
      await panel.getByText("People I trust (1)").waitFor({ timeout: 5000 });
      const on = await panel.locator("label", { hasText: "Autoplay media from the people below" }).locator("input").isChecked();
      const listed = await panel.innerText();
      await shot(A.page, "autoplay-settings");
      await closeDialogs();
      if (!on) throw new Error("the switch is still off");
      if (!listed.includes("Bob")) throw new Error("Bob is not on the list");
    });
    await check("a new video from Bob starts by itself, muted, in the chat (not floating)", async () => {
      await post(B, `${tag} second`, VIDEO);
      const r = await until(`${tag} second`, (x) => x.video && x.paused === false && x.t > 0.2, 60000, "did not start playing");
      await shot(A.page, "autoplay-playing");
      if (r.muted !== true) throw new Error("it is playing with sound");
      if (r.where !== "mp-docked" || r.floating) throw new Error(`it is ${r.where}, ${r.floating} floating`);
      return `playing at ${r.t.toFixed(1)}s, muted, in the message`;
    });
    await check("scrolled out of view it pauses and does not float; scrolled back it plays again", async () => {
      await A.page.evaluate(() => { const l = document.querySelector(".message-list"); l.scrollTop = 0; });
      const away = await until(`${tag} second`, (x) => x.paused === true, 10000, "kept playing out of view");
      if (away.floating) throw new Error("it floated over the chat");
      await A.page.evaluate(() => { const l = document.querySelector(".message-list"); l.scrollTop = l.scrollHeight; });
      const back = await until(`${tag} second`, (x) => x.paused === false, 10000, "did not resume");
      return `paused at ${away.t.toFixed(1)}s, resumed`;
    });
    await check("pressing on the video hands it to you: the sound is no longer forced off", async () => {
      const v = A.page.locator(".message", { hasText: `${tag} second` }).locator("video.mp-media");
      await v.dispatchEvent("pointerdown");
      const r = await until(`${tag} second`, (x) => x.muted === false, 5000, "still muted after taking over");
      return "unmuted";
    });
    await check("the same video from someone NOT on the list does not start (A's own)", async () => {
      await post(A, `${tag} mine`, VIDEO);
      await until(`${tag} mine`, (x) => x.slot, 45000, "no video card");
      await new Promise((r) => setTimeout(r, 6000));
      const r = await card(`${tag} mine`);
      if (r.video) throw new Error("it started: " + JSON.stringify(r));
    });
    await check("a YouTube link from Bob does not start by itself", async () => {
      await post(B, `${tag} tube`, YT);
      await new Promise((r) => setTimeout(r, 10000));
      const r = await card(`${tag} tube`);
      if (r.video || r.iframe) throw new Error("a player opened: " + JSON.stringify(r));
      return r.slot ? "card with a poster" : "no card (preview not shown)";
    });
    await check("with Data Saver on nothing autoplays, and the Autoplay settings say why", async () => {
      await openSettings("Privacy");
      await A.page.locator(".settings-content label", { hasText: /^\s*Data Saver\s*$/ }).locator("input").check();
      await A.page.locator(".settings-nav-item", { hasText: "Autoplay" }).click();
      await A.page.locator(".settings-content").getByText(/Data Saver is on right now/).waitFor({ timeout: 5000 });
      await closeDialogs();
      await post(B, `${tag} saver`, VIDEO);
      await new Promise((r) => setTimeout(r, 10000));
      const r = await card(`${tag} saver`);
      if (r.video) throw new Error("it started with Data Saver on: " + JSON.stringify(r));
    });
    await check("with the system's reduce-motion setting on nothing autoplays", async () => {
      await openSettings("Privacy");
      await A.page.locator(".settings-content label", { hasText: /^\s*Data Saver\s*$/ }).locator("input").uncheck();
      await closeDialogs();
      await A.page.emulateMedia({ reducedMotion: "reduce" });
      await post(B, `${tag} calm`, VIDEO);
      await until(`${tag} calm`, (x) => x.slot, 45000, "no video card");
      await new Promise((r) => setTimeout(r, 6000));
      const r = await card(`${tag} calm`);
      if (r.video) throw new Error("it started with reduce-motion on: " + JSON.stringify(r));
    });
    await check("control: with reduce-motion off again, Bob's next video does start", async () => {
      await A.page.emulateMedia({ reducedMotion: "no-preference" });
      await post(B, `${tag} again`, VIDEO);
      await until(`${tag} again`, (x) => x.video && x.paused === false, 60000, "did not start");
    });
    await check("pop-up: a direct video link from Bob shows a muted, playing preview in A's pop-up", async () => {
      // A direct media file on an allowlisted media host (the tweet's own mp4), with A's app in the background.
      const DIRECT = process.env.AUTOPLAY_DIRECT || "https://video.twimg.com/ext_tw_video/1585341912877146112/pu/vid/1920x1080/aeoVUvTgj4wHShhN.mp4";
      await A.page.evaluate(() => { document.hasFocus = () => false; });
      try {
        await sendText(B, `${tag} popup ${DIRECT}`);
        const pop = await pageMatching(A, /[?&]notify=1/, 30000);
        const card = pop.locator(".notify-popup-card", { hasText: `${tag} popup` }).first();
        await card.waitFor({ timeout: 30000 });
        const v = card.locator("video.notify-popup-media");
        await v.waitFor({ timeout: 45000 });
        const st = await pop.waitForFunction((sel) => { const e = document.querySelector(sel); return e && !e.paused && e.currentTime > 0.2 ? { muted: e.muted, loop: e.loop } : null; }, "video.notify-popup-media", { timeout: 30000 }).then((h) => h.jsonValue());
        await shot(pop, "A-popup-preview");
        if (!st.muted) throw new Error("the preview plays with sound");
        return `playing, muted${st.loop ? ", looping" : ""}`;
      } finally {
        await A.page.evaluate(() => { delete document.hasFocus; }).catch(() => {});
      }
    });
  } finally {
    await A.page.emulateMedia({ reducedMotion: null }).catch(() => {});
    await A.page.evaluate(() => { localStorage.removeItem("farder.autoplay"); window.dispatchEvent(new CustomEvent("farder:autoplay-changed")); }).catch(() => {});
    await closeDialogs().catch(() => {});
  }
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// An ENCRYPTED channel through the real app (run after `ubuntu`): the owner makes it in Server Settings, B is
// added by the app on its own, both send and read. Whether the server ever held the words is checked by
// ubuntu-suite.sh `e2ee` on the server's raw database bytes afterwards (E2EE_WORD is the secret marker).
async function e2ee() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const word = process.env.E2EE_WORD || ("sealed" + stamp);
  const chan = `secret-${stamp}`.toLowerCase();
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  for (const c of [A, B]) {
    if (await c.page.locator('input[aria-label="PIN entry"]').first().waitFor({ timeout: 3000 }).then(() => true, () => false)) await unlock(c);
    for (let i = 0; i < 3 && (await c.page.locator(".modal-close").count()) > 0; i++) await c.page.locator(".modal-close").first().click().catch(() => {});
    await c.page.locator(`.server-icon[title="${name}"]`).click();
  }
  if (!(await check("A (owner) makes an encrypted channel in Server Settings", async () => {
    await A.page.getByTitle("Server Settings").click();
    await A.page.getByPlaceholder("Channel name").fill(chan);
    await A.page.locator("label", { hasText: "Encrypted" }).locator('input[type="checkbox"]').check();
    await A.page.getByText(/Whoever runs this server cannot read/).waitFor({ timeout: 5000 });
    // The "Add" next to the channel name (the page also has Add buttons for categories and roles).
    await A.page.getByPlaceholder("Channel name").locator("xpath=ancestor::div[2]").getByRole("button", { name: "Add", exact: true }).click();
    await A.page.locator(".organizer-name", { hasText: chan }).first().waitFor({ timeout: 30000 });
    await A.page.locator(".modal-close").first().click();
  }))) return;
  await check("both apps show the channel with the lock", async () => {
    for (const c of [A, B]) {
      const item = c.page.locator(".channel-item", { hasText: chan }).first();
      await item.waitFor({ timeout: 60000 });
      await item.locator(".channel-e2ee-lock").waitFor({ timeout: 10000 });
    }
  });
  const sendUntilSeen = async (from, to, text) => {
    const t0 = Date.now();
    for (let i = 0; i < 18; i++) {
      const box = from.page.getByPlaceholder(/Type a message|Encrypted message/);
      if (await box.isEnabled().catch(() => false)) {
        await box.fill(text); await box.press("Enter");
        if (await to.page.locator(".message-content", { hasText: text }).first().waitFor({ timeout: 10000 }).then(() => true, () => false)) return `${Math.round((Date.now() - t0) / 1000)}s, try ${i + 1}`;
      } else await new Promise((r) => setTimeout(r, 5000));
    }
    const err = await from.page.locator(".error-text").first().innerText().catch(() => "");
    throw new Error("not seen within 3 minutes" + (err ? "; sender shows: " + err.slice(0, 120) : ""));
  };
  await check("A writes in the encrypted channel and B reads it", async () => {
    await A.page.locator(".channel-item", { hasText: chan }).first().click();
    await B.page.locator(".channel-item", { hasText: chan }).first().click();
    const t = await sendUntilSeen(A, B, `${word} from A`);
    await shot(B.page, "B-reads-encrypted");
    return `after ${t}`;
  });
  await check("B answers in the encrypted channel and A reads it", async () => {
    const t = await sendUntilSeen(B, A, `${word} from B`);
    await shot(A.page, "A-reads-encrypted");
    return `after ${t}`;
  });
  // Every message in the channel must open on both sides. A "couldn't decrypt" here was a real bug: the
  // first message after B was added arrived before B's Welcome and was cached as unreadable forever.
  await check("no message in the encrypted channel is shown as 'couldn't decrypt' on either side", async () => {
    await new Promise((r) => setTimeout(r, 5000));
    const early = [];
    for (const c of [A, B]) {
      const bad = await c.page.locator("em", { hasText: "couldn't decrypt" }).evaluateAll((els) => els.map((e) => e.title));
      if (bad.length) { await shot(c.page, `${c.who}-undecryptable`); throw new Error(`${c.who}: ${bad.length} unreadable: ${bad[0]}`); }
      const n = await c.page.locator("em", { hasText: "Sent before you could read this channel" }).count();
      if (n) { early.push(`${c.who}: ${n} sent before it could read`); await shot(c.page, `${c.who}-before-joined`); }
      if (await c.page.locator(".e2ee-unverified-marker").count()) throw new Error(`${c.who}: the 'could not be verified' warning shows`);
    }
    return early.join("; ") || "every message opened";
  });
  // A member's channel state must survive all of the above: publishing a key package over a confirmed state
  // resets it (the command now refuses to; this would catch a regression).
  await check("both apps end as confirmed members at the same epoch", async () => {
    const st = [];
    // A, B, then A again: one side's own processing may commit (advance the epoch); the other must follow.
    for (const c of [A, B, A]) {
      st.push(await c.page.evaluate(async ([nm, ch]) => {
        const inv = window.__TAURI_INTERNALS__.invoke;
        const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
        const info = await inv("get_server_info_v2", { serverId: s.id });
        const c = info.channels.map((x) => x.base || x).find((x) => x.name === ch);
        const r = await inv("process_mls_control_events", { serverId: s.id, logServerId: info.server_id, channelId: c.id });
        const leaves = await inv("e2ee_channel_leaves", { logServerId: info.server_id, channelId: c.id });
        return { confirmed: r.confirmed, epoch: r.epoch, outcome: r.outcome, processed: r.processed, leaves: leaves.length };
      }, [name, chan]));
    }
    const [a1, b1, a2] = st;
    if (!st.every((x) => x.confirmed && x.outcome === "advanced") || a2.epoch !== b1.epoch || a2.leaves !== b1.leaves) throw new Error(JSON.stringify(st));
    return `epoch ${b1.epoch} on both, ${b1.leaves} leaves (A first saw epoch ${a1.epoch})`;
  });
  // Whatever the epoch numbers say, messages must still flow both ways afterwards.
  await check("after all that, a further message each way still opens", async () => {
    const a = await sendUntilSeen(B, A, `${word} later from B`);
    const b = await sendUntilSeen(A, B, `${word} later from A`);
    for (const c of [A, B]) if (await c.page.locator("em", { hasText: "couldn't decrypt" }).count()) throw new Error(`${c.who} shows couldn't decrypt`);
    return `B->A ${a}, A->B ${b}`;
  });
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

// Diagnosis for a failing encrypted channel (run after `e2ee` on the same stack): per app, what the
// screen says and what the channel's encryption state is; then the owner's add, with its skip reasons.
async function e2eeDiag() {
  const name = process.env.UBU_NAME || "Ubuntu Server";
  const { stamp } = JSON.parse(fs.readFileSync(process.env.STATE || "selftest-state.json", "utf8"));
  const chan = `secret-${stamp}`.toLowerCase();
  const A = await attach(PORT_A, "A");
  const B = await attach(PORT_B, "B");
  const probe = (c, doAdd) => c.page.evaluate(async ([nm, ch, add]) => {
    const inv = window.__TAURI_INTERNALS__.invoke;
    const out = {};
    try {
      const s = (await inv("get_saved_servers")).find((x) => x.name === nm);
      const info = await inv("get_server_info_v2", { serverId: s.id });
      out.logServerId = info.server_id ?? info.log_server_id ?? null;
      const c = info.channels.map((x) => ({ ...(x.base || x), class: x.class })).find((x) => x.name === ch);
      out.channel = c ? { id: c.id, class: c.class } : null;
      if (c && out.logServerId) {
        try { out.leaves = await inv("e2ee_channel_leaves", { logServerId: out.logServerId, channelId: c.id }); } catch (e) { out.leavesErr = String(e); }
        try { out.process = await inv("process_mls_control_events", { serverId: s.id, logServerId: out.logServerId, channelId: c.id }); } catch (e) { out.processErr = String(e); }
        if (add) { try { out.add = await inv("add_members_to_e2ee_channel", { serverId: s.id, logServerId: out.logServerId, channelId: c.id }); } catch (e) { out.addErr = String(e); } }
      }
    } catch (e) { out.err = String(e); }
    out.screen = (document.querySelector(".chat-panel")?.innerText || "").slice(0, 600);
    return out;
  }, [name, chan, doAdd]);
  for (const [c, add] of [[A, true], [B, false], [A, false]]) {
    await c.page.locator(".channel-item", { hasText: chan }).first().click().catch(() => {});
    await new Promise((r) => setTimeout(r, 3000));
    await shot(c.page, `${c.who}-e2ee-diag`);
    console.log(`=== ${c.who}${add ? " (with owner add)" : ""}: ` + JSON.stringify(await probe(c, add), null, 1).slice(0, 3000));
  }
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

const scenario = process.argv[2] || "core";
try {
  if (scenario === "core") await core();
  else if (scenario === "relaunch") await relaunch();
  else if (scenario === "buddy") await buddy();
  else if (scenario === "dm") await dm();
  else if (scenario === "edit") await edit();
  else if (scenario === "direct") await direct();
  else if (scenario === "unread") await unread();
  else if (scenario === "popup") await popup();
  else if (scenario === "themes") await themes();
  else if (scenario === "attach") await attach_file();
  else if (scenario === "garble") await garble();
  else if (scenario === "tray") await tray();
  else if (scenario === "powers") await powers();
  else if (scenario === "ubuntu") await ubuntu();
  else if (scenario === "ubuntu-restored") await ubuntuRestored();
  else if (scenario === "server-down") await serverDown();
  else if (scenario === "recovery") await recovery();
  else if (scenario === "host-reset") await hostReset();
  else if (scenario === "guest-link") await guestLink();
  else if (scenario === "brake") await brake();
  else if (scenario === "failover") await failover();
  else if (scenario === "e2ee") await e2ee();
  else if (scenario === "density") await density();
  else if (scenario === "smallwin") await smallwin();
  else if (scenario === "autoplay") await autoplay();
  else if (scenario === "e2ee-diag") await e2eeDiag();
  else if (scenario === "voice") await voice();
  else if (scenario === "popup-size") await popupSize();
  else if (scenario === "phone") await phone();
  else if (scenario === "admin-garble") await adminGarble();
  else if (scenario === "strip") await strip();
  else if (scenario === "direct-relaunch") await directRelaunch();
  else throw new Error("unknown scenario " + scenario);
} catch (e) {
  log("FAIL", "driver", String(e.message || e));
}
const failed = results.filter((r) => r.status === "FAIL").length;
console.log(`\n${results.length - failed} passed, ${failed} failed`);
fs.writeFileSync(process.env.RESULTS || "selftest-results.json", JSON.stringify(results, null, 2));
process.exit(failed ? 1 : 0);
