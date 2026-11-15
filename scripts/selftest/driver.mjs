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
async function check(name, fn) {
  try { const d = await fn(); log("PASS", name, d ?? ""); return true; }
  catch (e) { log("FAIL", name, String(e.message || e).split("\n")[0]); return false; }
}
async function shot(page, tag) {
  const dir = process.env.SHOTS || "selftest-shots";
  fs.mkdirSync(dir, { recursive: true });
  const f = `${dir}/${String(++shotN).padStart(2, "0")}-${tag}.png`;
  await page.screenshot({ path: f }).catch(() => {});
  return f;
}

async function attach(port, who) {
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
  return { browser, ctx, page, who };
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
  await check("A: earlier history is still there", () => seesText(A, "hello from B " + stamp));
  await check("B: earlier history is still there", () => seesText(B, "hello from A " + stamp));
  await check("A: send after relaunch, B sees it", async () => { await sendText(A, "after relaunch A " + stamp); await seesText(B, "after relaunch A " + stamp); });
  await check("B: send after relaunch, A sees it", async () => { await sendText(B, "after relaunch B " + stamp); await seesText(A, "after relaunch B " + stamp); });
  await shot(A.page, "A-relaunch");
  await shot(B.page, "B-relaunch");
  await A.browser.close().catch(() => {});
  await B.browser.close().catch(() => {});
}

const scenario = process.argv[2] || "core";
try {
  if (scenario === "core") await core();
  else if (scenario === "relaunch") await relaunch();
  else throw new Error("unknown scenario " + scenario);
} catch (e) {
  log("FAIL", "driver", String(e.message || e));
}
const failed = results.filter((r) => r.status === "FAIL").length;
console.log(`\n${results.length - failed} passed, ${failed} failed`);
fs.writeFileSync(process.env.RESULTS || "selftest-results.json", JSON.stringify(results, null, 2));
process.exit(failed ? 1 : 0);
