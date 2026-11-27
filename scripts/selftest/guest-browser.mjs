// The browser guest page, in a real (Edge) browser, against a real server through the gateway.
// Needs the fixture running in WSL:  cargo run -p farder-gateway --example guest_fixture
// plus a static server for the website folder:  (cd website && python3 -m http.server 8099 --bind 127.0.0.1)
// Run from Windows:  node scripts\selftest\guest-browser.mjs <code-from-fixture>
import { chromium } from "playwright-core";
import fs from "node:fs";

// Argument: a bare guest code (fixture mode), or the full link the app made (https://host/<name>/t#<code>),
// which is mapped to the local page and gateway. Env: GUEST_SERVER_NAME / GUEST_WELCOME for what to expect.
let code = process.argv[2];
if (!code) { console.log("usage: node guest-browser.mjs <guest code or link>"); process.exit(2); }
let route = "friends";
const m = code.match(/^https?:\/\/[^/]+\/([A-Za-z0-9_-]+)\/t#(.+)$/);
if (m) { route = m[1]; code = m[2]; }
const EXPECT_SERVER = process.env.GUEST_SERVER_NAME || "Friends Server";
const EXPECT_WELCOME = process.env.GUEST_WELCOME || "Welcome to the friends server!";
const BASE = `http://127.0.0.1:8099/guest/index.html?server=${route}&gateway=ws://127.0.0.1:8480/ws/${route}`;
const SHOTS = process.env.SHOTS || ".";
fs.mkdirSync(SHOTS, { recursive: true });
let pass = 0, fail = 0;
async function check(name, fn) {
  try { const r = await fn(); pass++; console.log("PASS " + name + (r ? "  -- " + r : "")); return true; }
  catch (e) { fail++; console.log("FAIL " + name + "  -- " + String(e.message || e).split("\n")[0].slice(0, 200)); return false; }
}
const browser = await chromium.launch({ executablePath: "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", headless: true });
const ctx = await browser.newContext({ viewport: { width: 1000, height: 700 } });
const page = await ctx.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });

await check("the join screen loads (WebAssembly starts) and explains what a guest can do", async () => {
  await page.goto(BASE + "#" + code);
  await page.locator("#join").waitFor({ timeout: 20000 });
  await page.locator("#name").waitFor();
  const text = await page.locator("#join").textContent();      // includes the folded-away details
  if (!/gateway/i.test(text) || !/can see the messages/i.test(text)) throw new Error("the privacy note is missing");
  await page.screenshot({ path: `${SHOTS}/guest-join.png` });
});
await check("with a name and the guest link the guest gets into the chat", async () => {
  await page.locator("#name").fill("Webby");
  await page.locator("#go").click();
  await page.locator("#chat").waitFor({ state: "visible", timeout: 30000 });
  await page.locator("#status", { hasText: "connected as a guest" }).waitFor({ timeout: 30000 });
  if ((await page.locator("#server-name").innerText()) !== EXPECT_SERVER) throw new Error("wrong server name");
});
await check("the guest sees the earlier message in #general", async () => {
  await page.locator(".msg", { hasText: EXPECT_WELCOME }).waitFor({ timeout: 20000 });
  await page.screenshot({ path: `${SHOTS}/guest-chat.png` });
});
await check("the guest can write, and sees their own message under their chosen name", async () => {
  await page.locator("#text").fill("hello from a browser guest");
  await page.locator("#composer button").click();
  const m = page.locator(".msg", { hasText: "hello from a browser guest" });
  await m.waitFor({ timeout: 20000 });
  const who = await m.locator(".who").innerText();
  if (who !== "Webby") throw new Error("shown as " + who);
});
await check("messages are shown as text, never as HTML", async () => {
  await page.locator("#text").fill("<img src=x onerror=window.__pwned=1> <b>bold?</b>");
  await page.locator("#composer button").click();
  await page.locator(".msg", { hasText: "<b>bold?</b>" }).waitFor({ timeout: 20000 });
  if (await page.evaluate(() => window.__pwned)) throw new Error("script ran");
  if (await page.locator("#messages img, #messages b").count()) throw new Error("HTML was rendered");
});
await check("a refresh keeps the same guest (key in this browser): straight back in, history intact", async () => {
  await page.reload();
  await page.locator("#join").waitFor({ timeout: 20000 });
  if ((await page.locator("#name").inputValue()) !== "Webby") throw new Error("name not remembered");
  await page.locator("#go").click();
  await page.locator(".msg", { hasText: "hello from a browser guest" }).waitFor({ timeout: 30000 });
});
await check("a second person (new browser profile) joins with the same link and sees the first guest's line", async () => {
  const ctx2 = await browser.newContext();
  const p2 = await ctx2.newPage();
  await p2.goto(BASE + "#" + code);
  await p2.locator("#name").fill("Second");
  await p2.locator("#go").click();
  await p2.locator(".msg", { hasText: "hello from a browser guest" }).waitFor({ timeout: 30000 });
  // live: the first guest writes, the second sees it without reloading
  await page.locator("#text").fill("live line from Webby");
  await page.locator("#composer button").click();
  await p2.locator(".msg", { hasText: "live line from Webby" }).waitFor({ timeout: 20000 });
  await ctx2.close();
});
await check("a wrong link code is refused with a plain explanation", async () => {
  const ctx3 = await browser.newContext();
  const p3 = await ctx3.newPage();
  await p3.goto(BASE + "#WRONGCODE");
  await p3.locator("#name").fill("Nobody");
  await p3.locator("#go").click();
  await p3.locator("#problem").waitFor({ state: "visible", timeout: 20000 });
  const t = await p3.locator("#problem").innerText();
  if (!/no longer works/i.test(t)) throw new Error("unexpected: " + t.slice(0, 120));
  await p3.screenshot({ path: `${SHOTS}/guest-bad-link.png` });
  await ctx3.close();
});
// A phone: an emulated iPhone (small screen, touch, mobile browser). Not a real device, but it catches
// the layout problems a phone would show: anything wider than the screen, a composer pushed off it,
// buttons too small to tap.
await check("on a phone-sized screen: joins, nothing runs off the side, the box and Send fit and are big enough to tap, sending works", async () => {
  const { devices } = await import("playwright-core");
  const ctxP = await browser.newContext({ ...devices["iPhone 13"] });
  const pp = await ctxP.newPage();
  pp.on("pageerror", (e) => errors.push("phone: " + String(e)));
  await pp.goto(BASE + "#" + code);
  await pp.locator("#name").fill("Phoney");
  await pp.locator("#go").click();
  await pp.locator(".msg", { hasText: "hello from a browser guest" }).waitFor({ timeout: 30000 });
  const r = await pp.evaluate(() => {
    const box = document.querySelector("#text").getBoundingClientRect();
    const send = document.querySelector("#composer button").getBoundingClientRect();
    const wide = [...document.querySelectorAll("body *")].filter((e) => { const b = e.getBoundingClientRect(); return b.width > 0 && b.right > innerWidth + 1; }).slice(0, 3).map((e) => e.id || e.className || e.tagName);
    return { vw: innerWidth, vh: innerHeight, sideways: document.documentElement.scrollWidth > innerWidth + 1, wide,
      box: [Math.round(box.left), Math.round(box.right), Math.round(box.bottom), Math.round(box.height)],
      send: [Math.round(send.right), Math.round(send.bottom), Math.round(send.width), Math.round(send.height)],
      font: parseFloat(getComputedStyle(document.querySelector(".msg")).fontSize) };
  });
  await pp.screenshot({ path: `${SHOTS}/guest-phone.png` });
  if (r.sideways || r.wide.length) throw new Error("wider than the phone: " + JSON.stringify(r.wide));
  if (r.box[1] > r.vw || r.box[2] > r.vh || r.box[3] < 30) throw new Error("the message box does not fit: " + JSON.stringify(r));
  if (r.send[0] > r.vw || r.send[1] > r.vh) throw new Error("Send is off the screen: " + JSON.stringify(r));
  if (r.send[2] < 40 || r.send[3] < 32) throw new Error(`Send is too small to tap (${r.send[2]}x${r.send[3]})`);
  if (r.font < 13) throw new Error(`message text is ${r.font}px on a phone`);
  await pp.locator("#text").fill("hello from a phone");
  await pp.locator("#composer button").tap();
  await pp.locator(".msg", { hasText: "hello from a phone" }).waitFor({ timeout: 20000 });
  await ctxP.close();
  return `${r.vw}x${r.vh}, Send ${r.send[2]}x${r.send[3]}, text ${r.font}px`;
});
await check("no script errors on the page", async () => { if (errors.length) throw new Error(errors.slice(0, 2).join(" | ")); });
await browser.close();
console.log(`${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
