import test from "node:test";
import assert from "node:assert/strict";
const r = await import(process.env.REPLYTARGET_JS);

const msgs = [{ id: 1, event_hash: "abc" }, { id: 2, event_hash: null }, { id: 3 }];

test("no reply requested is a plain post", () => {
  assert.deepEqual(r.chooseReplyTarget(undefined, msgs), { replyTo: null, tooOld: false });
});
test("a target with a hash returns the hash", () => {
  assert.deepEqual(r.chooseReplyTarget(1, msgs), { replyTo: "abc", tooOld: false });
});
test("a target without a hash is flagged too old", () => {
  assert.deepEqual(r.chooseReplyTarget(2, msgs), { replyTo: null, tooOld: true });
  assert.deepEqual(r.chooseReplyTarget(3, msgs), { replyTo: null, tooOld: true });
});
test("a target not in state is flagged too old", () => {
  assert.deepEqual(r.chooseReplyTarget(99, msgs), { replyTo: null, tooOld: true });
});
