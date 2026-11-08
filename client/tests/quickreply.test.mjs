import test from "node:test";
import assert from "node:assert/strict";
const q = await import(process.env.QUICKREPLY_JS);

const view = {
  channels: [{ id: 1, class: "Plaintext" }, { id: 2, class: "E2ee" }, { id: 3 }],
  dms: [{ channel: { id: 50 }, peerKey: "vk_peer" }],
  logServerId: "log1",
};
const req = (over = {}) => ({ id: "r1", serverId: "s1", channelId: 1, text: "hello", ...over });

test("a plain channel on a mesh server plans a mesh send", () => {
  const p = q.planReply(req(), view);
  assert.deepEqual(p, { ok: true, text: "hello", plan: { kind: "mesh", logServerId: "log1" } });
});

test("a plain channel on a legacy server plans a legacy send", () => {
  const p = q.planReply(req(), { ...view, logServerId: null });
  assert.equal(p.plan.kind, "legacy");
});

test("a channel with no class is plaintext", () => {
  assert.equal(q.planReply(req({ channelId: 3 }), view).ok, true);
});

test("a DM is planned as a DM, even on a mesh server", () => {
  const p = q.planReply(req({ channelId: 50 }), view);
  assert.deepEqual(p.plan, { kind: "dm", peerKey: "vk_peer" });
});

test("an encrypted channel is refused rather than sent as plaintext", () => {
  const p = q.planReply(req({ channelId: 2 }), view);
  assert.equal(p.ok, false);
  assert.match(p.error, /encrypted/);
});

test("things that do not exist are rejected, not guessed at", () => {
  assert.equal(q.planReply(req({ channelId: 999 }), view).ok, false);
  assert.equal(q.planReply(req(), undefined).ok, false);
});

test("untrusted payloads are rejected", () => {
  for (const bad of [null, undefined, 5, "x", {}, req({ id: "" }), req({ id: "x".repeat(65) }), req({ serverId: 7 }),
    req({ channelId: "1" }), req({ channelId: 1.5 }), req({ channelId: -1 }), req({ text: 9 })]) {
    assert.equal(q.planReply(bad, view).ok, false, JSON.stringify(bad));
  }
});

test("empty, whitespace-only and over-long text is refused; control characters are stripped", () => {
  assert.equal(q.planReply(req({ text: "   " }), view).ok, false);
  assert.equal(q.planReply(req({ text: "\u0007\n" }), view).ok, false);
  assert.equal(q.planReply(req({ text: "x".repeat(q.MAX_REPLY_CHARS + 1) }), view).ok, false);
  assert.equal(q.planReply(req({ text: "x".repeat(q.MAX_REPLY_CHARS) }), view).ok, true);
  assert.equal(q.planReply(req({ text: " hi\u0007 there\n" }), view).text, "hi there");
});

function deps() {
  const calls = [];
  return {
    calls,
    d: {
      sendMessage: async (...a) => { calls.push(["sendMessage", ...a]); },
      submitEvent: async (...a) => { calls.push(["submitEvent", ...a]); },
      dmEncrypt: async (peer, t) => { calls.push(["dmEncrypt", peer, t]); return "CIPHER(" + t + ")"; },
    },
  };
}

test("a DM reply is encrypted before it is sent and the plaintext never goes out", async () => {
  const { calls, d } = deps();
  await q.executeReply({ kind: "dm", peerKey: "vk_peer" }, req({ channelId: 50, text: "secret" }), "secret", d);
  assert.deepEqual(calls, [["dmEncrypt", "vk_peer", "secret"], ["sendMessage", "s1", 50, "CIPHER(secret)"]]);
  assert.ok(!calls.some((c) => c[0] === "sendMessage" && c[3] === "secret"));
});

test("if DM encryption fails nothing is sent", async () => {
  const calls = [];
  const d = {
    sendMessage: async (...a) => { calls.push(a); },
    submitEvent: async (...a) => { calls.push(a); },
    dmEncrypt: async () => { throw new Error("no identity"); },
  };
  await assert.rejects(q.executeReply({ kind: "dm", peerKey: "p" }, req({ channelId: 50 }), "secret", d));
  assert.equal(calls.length, 0);
});

test("a mesh reply goes through submitEvent with no reply-to and no attachments", async () => {
  const { calls, d } = deps();
  await q.executeReply({ kind: "mesh", logServerId: "log1" }, req(), "hello", d);
  assert.deepEqual(calls, [["submitEvent", "s1", "log1", 1, "hello", null, []]]);
});

test("a legacy reply goes through sendMessage", async () => {
  const { calls, d } = deps();
  await q.executeReply({ kind: "legacy" }, req(), "hello", d);
  assert.deepEqual(calls, [["sendMessage", "s1", 1, "hello"]]);
});
