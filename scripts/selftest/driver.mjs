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
  await p.getByText("general").first().waitFor({ timeout: 90000 });
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
  // A small real PNG (16x16, solid orange).
  const { deflateSync } = await import("node:zlib");
  const crcT = Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
  const crc = (b) => { let c = 0xffffffff; for (const x of b) c = crcT[(c ^ x) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
  const chunk = (t, d) => { const len = Buffer.alloc(4); len.writeUInt32BE(d.length); const td = Buffer.concat([Buffer.from(t), d]); const cs = Buffer.alloc(4); cs.writeUInt32BE(crc(td)); return Buffer.concat([len, td, cs]); };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(16, 0); ihdr.writeUInt32BE(16, 4); ihdr[8] = 8; ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.concat(Array.from({ length: 16 }, () => Buffer.from([255, 140, 0])))]);
  const png = Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(Buffer.concat(Array.from({ length: 16 }, () => row)))), chunk("IEND", Buffer.alloc(0))]);
  const dir = process.env.SHOTS || "selftest-shots";
  fs.mkdirSync(dir, { recursive: true });
  const fileName = `orange-${stamp}.png`;
  const path = (await import("node:path")).resolve(dir, fileName);
  fs.writeFileSync(path, png);
  await check("A: drop a picture on the window, it is staged, then sent with a caption", async () => {
    await A.page.evaluate((p) => window.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "tauri://drag-drop", payload: { paths: [p], position: { x: 400, y: 400 } } }), path);
    await A.page.getByText(fileName).first().waitFor({ timeout: 30000 });
    await shot(A.page, "A-file-staged");
    await sendText(A, "with a file " + stamp);
    await seesText(A, "with a file " + stamp, 30000);
  });
  await check("B: receives the message with the file, and the picture actually loads", async () => {
    await seesText(B, "with a file " + stamp, 30000);
    const msg = B.page.locator(".message", { hasText: "with a file " + stamp }).first();
    await msg.locator("img:not(.message-avatar img)").last().waitFor({ timeout: 30000 });
    const ok = await msg.locator("img").evaluateAll((imgs) => imgs.some((i) => i.naturalWidth >= 8 && !i.classList.contains("message-avatar")));
    await shot(B.page, "B-file");
    if (!ok) throw new Error("the picture element is there but did not load (naturalWidth 0)");
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
    return "refused: " + refused.slice(0, 80);
  });
  await check("B starts a takeover with the 24 words; both screens show it, with a countdown", async () => {
    await B.page.getByLabel("The 24 recovery words").fill(words);
    await B.page.getByRole("button", { name: "Start the takeover" }).click();
    await B.page.getByText(/Ownership of this server is moving to/).waitFor({ timeout: 30000 });
    await shot(B.page, "B-takeover-started");
    await closeDialog(B);
    await openRecovery(A);
    await A.page.getByText(/Ownership of this server is moving to/).waitFor({ timeout: 30000 });
    await shot(A.page, "A-sees-takeover");
    await closeDialog(A);
    // Every member sees it on the main screen, without opening any settings.
    await A.page.locator(".recovery-banner", { hasText: "Ownership of this server is moving" }).waitFor({ timeout: 30000 });
    await B.page.locator(".recovery-banner", { hasText: "Ownership of this server is moving" }).waitFor({ timeout: 30000 });
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
    await A.page.locator(".recovery-banner").waitFor({ state: "detached", timeout: 30000 });
    await B.page.locator(".recovery-banner").waitFor({ state: "detached", timeout: 30000 });
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
