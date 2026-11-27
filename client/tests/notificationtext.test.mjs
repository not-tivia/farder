import test from "node:test";
import assert from "node:assert/strict";
const n = await import(process.env.NOTIFTEXT_JS);

const parts = { sender: "Alice", channel: "plans", text: "the secret meeting is at 9 in the old warehouse" };

test("full shows who, where and the words", () => {
  const r = n.buildNotificationText("full", parts);
  assert.equal(r.title, "Alice in #plans");
  assert.equal(r.body, parts.text);
  assert.equal(r.allowMedia, true);
});

test("sender-only never contains the words or the channel", () => {
  const r = n.buildNotificationText("sender", parts);
  assert.equal(r.body, "New message from Alice");
  assert.ok(!JSON.stringify(r).includes("secret") && !JSON.stringify(r).includes("plans"));
  assert.equal(r.allowMedia, false);
});

test("hidden contains nothing about the message at all", () => {
  const r = n.buildNotificationText("hidden", parts);
  assert.deepEqual(r, { title: "Farder", body: "New message", allowMedia: false });
  for (const leak of ["Alice", "plans", "secret", "warehouse"]) assert.ok(!JSON.stringify(r).includes(leak), leak);
});

test("an unknown or corrupt mode falls back to full, never to something stricter by accident or weaker by crash", () => {
  assert.equal(n.normalizePopupContent(undefined), "full");
  assert.equal(n.normalizePopupContent("nonsense"), "full");
  assert.equal(n.normalizePopupContent(5), "full");
  assert.equal(n.normalizePopupContent("sender"), "sender");
  assert.equal(n.normalizePopupContent("hidden"), "hidden");
});

test("only the full mode may carry a media preview", () => {
  for (const m of ["sender", "hidden"]) assert.equal(n.buildNotificationText(m, parts).allowMedia, false);
});

test("long text is cut and names are cleaned of control characters and length", () => {
  const long = "x".repeat(500);
  assert.equal(n.buildNotificationText("full", { text: long }).body.length, 120);
  const r = n.buildNotificationText("sender", { sender: "Al\nice\u0007" + "y".repeat(200), text: "t" });
  assert.ok(!/[\n\u0007]/.test(r.body));
  assert.ok(r.body.length <= "New message from ".length + 64, "the name is capped at 64 characters");
});

test("missing names degrade gracefully", () => {
  assert.equal(n.buildNotificationText("sender", { text: "t" }).body, "New message");
  assert.equal(n.buildNotificationText("full", { text: "hi" }).title, "Farder");
});
