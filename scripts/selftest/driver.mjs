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
  page.on("console", (m) => { const t = m.text(); if (/^\[(chat|buddy)\]/.test(t)) logs.push(`${new Date().toISOString().slice(11, 19)} ${t.slice(0, 160)}`); });
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

const scenario = process.argv[2] || "core";
try {
  if (scenario === "core") await core();
  else if (scenario === "relaunch") await relaunch();
  else if (scenario === "buddy") await buddy();
  else if (scenario === "dm") await dm();
  else if (scenario === "edit") await edit();
  else if (scenario === "direct") await direct();
  else if (scenario === "direct-relaunch") await directRelaunch();
  else throw new Error("unknown scenario " + scenario);
} catch (e) {
  log("FAIL", "driver", String(e.message || e));
}
const failed = results.filter((r) => r.status === "FAIL").length;
console.log(`\n${results.length - failed} passed, ${failed} failed`);
fs.writeFileSync(process.env.RESULTS || "selftest-results.json", JSON.stringify(results, null, 2));
process.exit(failed ? 1 : 0);
