import test from "node:test";
import assert from "node:assert/strict";
const r = await import(process.env.RECOVERY_JS);

test("a wait is described in the two largest units, with the right plurals", () => {
  assert.equal(r.describeWait(72 * 3600), "3 days");
  assert.equal(r.describeWait(72 * 3600 + 4 * 3600), "3 days 4 hours");
  assert.equal(r.describeWait(86400 + 3600), "1 day 1 hour");
  assert.equal(r.describeWait(5 * 3600 + 10 * 60), "5 hours 10 minutes");
  assert.equal(r.describeWait(3600), "1 hour");
  assert.equal(r.describeWait(12 * 60), "12 minutes");
  assert.equal(r.describeWait(59), "less than a minute");
  assert.equal(r.describeWait(-5), "less than a minute");
});

test("a takeover is waiting until the server's ready time, then ready", () => {
  const p = { claim: "c", new_owner: "vk_x", accepted_at: 1000, ready_at: 1000 + 3600 };
  assert.deepEqual(r.takeoverState(null, 5000), { kind: "none" });
  assert.deepEqual(r.takeoverState(p, 1000), { kind: "waiting", left: 3600 });
  assert.deepEqual(r.takeoverState(p, 4599), { kind: "waiting", left: 1 });
  assert.deepEqual(r.takeoverState(p, 4600), { kind: "ready" });
  assert.deepEqual(r.takeoverState(p, 99999), { kind: "ready" });
});

test("pasted words are cleaned up and counted", () => {
  const words = Array.from({ length: 24 }, (_, i) => "word" + String.fromCharCode(97 + (i % 26))).join(" ");
  assert.equal(r.looksLikeRecoveryPhrase("abandon ".repeat(24)), true);
  assert.equal(r.looksLikeRecoveryPhrase("  Abandon\n\nABANDON, " + "abandon ".repeat(22)), true);
  assert.equal(r.looksLikeRecoveryPhrase("abandon ".repeat(23)), false);
  assert.equal(r.looksLikeRecoveryPhrase("abandon ".repeat(25)), false);
  assert.equal(r.normalizePhrase("  One,  TWO\nthree "), "one two three");
  assert.ok(words.length > 0);
});
