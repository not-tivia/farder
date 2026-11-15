// Invite and setup links, especially the ones that go through the relay.
import test from "node:test";
import assert from "node:assert/strict";
const { parseInviteLink } = await import(process.env.INVITE_JS);

const SID = "ab".repeat(32);
const FP = "cd".repeat(32);

test("a relay link with a setup token gives the token separately and leaves no invite code in the address", () => {
  const compact = parseInviteLink(`farder://relayd/${SID}/setup:${"1f".repeat(32)}`);
  assert.equal(compact.setupToken, "1f".repeat(32));
  assert.equal(compact.inviteCode, undefined);
  assert.equal(compact.address, `farder://relayd/${SID}`);
  const full = parseInviteLink(`farder://relay/45.61.136.29:4433/${SID}/${FP}/setup:${"2e".repeat(32)}`);
  assert.equal(full.setupToken, "2e".repeat(32));
  assert.equal(full.inviteCode, undefined);
  assert.equal(full.address, `farder://relay/45.61.136.29:4433/${SID}/${FP}/`);
});

test("an ordinary relay invite still keeps its code in the address", () => {
  const c = parseInviteLink(`farder://relayd/${SID}/CODE123`);
  assert.equal(c.address, `farder://relayd/${SID}/CODE123`);
  assert.equal(c.inviteCode, "CODE123");
  assert.equal(c.setupToken, undefined);
  const f = parseInviteLink(`farder://relay/1.2.3.4:4433/${SID}/${FP}/CODE456`);
  assert.equal(f.inviteCode, "CODE456");
  assert.equal(f.setupToken, undefined);
});

test("direct setup and invite links are unchanged", () => {
  assert.deepEqual(parseInviteLink("farder://10.0.0.5:4435/setup:aabb"), { address: "10.0.0.5:4435", setupToken: "aabb" });
  assert.deepEqual(parseInviteLink("farder://10.0.0.5:4435/CODE"), { address: "10.0.0.5:4435", inviteCode: "CODE" });
  assert.deepEqual(parseInviteLink("host:4435/setup:aabb"), { address: "host:4435", setupToken: "aabb" });
});

test("a web link wrapping a relay setup link is understood too", () => {
  const inner = `farder://relayd/${SID}/setup:${"3d".repeat(32)}`;
  const b64 = Buffer.from(inner).toString("base64url");
  const p = parseInviteLink(`https://farder.xyz/join/${b64}`);
  assert.equal(p.setupToken, "3d".repeat(32));
  assert.equal(p.address, `farder://relayd/${SID}`);
});

test("a link from another host is not borrowed", () => {
  const evil = parseInviteLink(`https://evil.example/farder.xyz/join/${Buffer.from("1.2.3.4:4435/setup:aa").toString("base64url")}`);
  assert.equal(evil.address, undefined, "must not be read as a join link for our host");
  assert.equal(evil.setupToken, undefined);
});
