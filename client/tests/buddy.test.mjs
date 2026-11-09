import test from "node:test";
import assert from "node:assert/strict";
const b = await import(process.env.BUDDY_JS);

const pk = (n) => ({ bytes: Array.from({ length: 32 }, () => n) });
const hex = (n) => "vk_" + Array.from({ length: 32 }, () => n.toString(16).padStart(2, "0")).join("");
const ME = pk(1), MARA = pk(2);
const msg = (id, channel, author, content, over = {}) => ({
  id, channel_id: channel, author, content, timestamp: 1000 + id, edited_at: null, reply_to: null,
  pinned: false, attachments: [], reactions: [], thread_id: null, thread_message_count: 0, ...over,
});
const view = (over = {}) => ({
  serverName: "Home",
  channels: [
    { id: 1, name: "general", position: 1, channel_type: "Text" },
    { id: 2, name: "voice", position: 2, channel_type: "Voice" },
    { id: 3, name: "secret", position: 3, channel_type: "Text", class: "E2ee" },
  ],
  dms: [{ channel: { id: 10 }, participant: { public_key: MARA, display_name: "Mara" }, last_message: null }],
  members: [{ public_key: MARA, display_name: "Mara" }, { public_key: ME, display_name: "chris" }],
  messages: {
    1: [msg(1, 1, MARA, "hi"), msg(2, 1, ME, "yo"), msg(3, 1, MARA, "there")],
    10: [msg(5, 10, MARA, "dm one"), msg(6, 10, MARA, "dm two")],
  },
  readState: { 1: 1 },
  currentChannelId: null,
  ...over,
});

test("the list has people first, then text channels, and skips voice channels", () => {
  const s = b.buildBuddyState({ a: view() }, ["a"], hex(1));
  assert.deepEqual(s.entries.map((e) => [e.kind, e.label]), [["person", "Mara"], ["channel", "general"], ["channel", "secret"]]);
  assert.equal(s.entries[2].encrypted, true);
});

test("unread counts other people's newer messages only, and not the channel being viewed", () => {
  const s = b.buildBuddyState({ a: view() }, ["a"], hex(1));
  const by = Object.fromEntries(s.entries.map((e) => [e.label, e.unread]));
  assert.equal(by.Mara, 2);      // both DM messages are unread
  assert.equal(by.general, 1);   // message 2 is mine, 1 is read, 3 is new
  const viewing = b.buildBuddyState({ a: view({ currentChannelId: 10 }) }, ["a"], hex(1));
  assert.equal(viewing.entries.find((e) => e.label === "Mara").unread, 0);
});

test("unread stops at 99", () => {
  const many = Array.from({ length: 150 }, (_, i) => msg(i + 1, 1, MARA, "x"));
  const s = b.buildBuddyState({ a: view({ messages: { 1: many }, readState: {} }) }, ["a"], hex(1));
  assert.equal(s.entries.find((e) => e.label === "general").unread, 99);
});

test("people from every server are listed, the one who wrote last on top", () => {
  const a = view({ serverName: "A", messages: { 10: [msg(5, 10, MARA, "old")] } });
  const c = view({ serverName: "C", dms: [{ channel: { id: 20 }, participant: { public_key: pk(3), display_name: "Dev" }, last_message: null }], messages: { 20: [msg(50, 20, pk(3), "new")] } });
  const s = b.buildBuddyState({ a, c }, ["a", "c"], hex(1));
  assert.deepEqual(s.entries.filter((e) => e.kind === "person").map((e) => e.label), ["Dev", "Mara"]);
});

test("a transcript names people, marks my lines and ends at the newest message", () => {
  const c = b.buildChatState(view(), "a", 1, hex(1));
  assert.equal(c.title, "#general");
  assert.deepEqual(c.lines.map((l) => [l.author, l.mine, l.text]), [["Mara", false, "hi"], ["You", true, "yo"], ["Mara", false, "there"]]);
  assert.equal(c.last_id, 3);
  assert.equal(c.can_send, true);
});

test("an encrypted channel can be read but not answered from a small window", () => {
  const c = b.buildChatState(view(), "a", 3, hex(1));
  assert.equal(c.can_send, false);
  assert.match(c.reason, /encrypted/);
});

test("a conversation that does not exist gives null, not a guess", () => {
  assert.equal(b.buildChatState(view(), "a", 999, hex(1)), null);
  assert.equal(b.buildChatState(undefined, "a", 1, hex(1)), null);
});

test("only the newest messages are kept", () => {
  const many = Array.from({ length: 200 }, (_, i) => msg(i + 1, 1, MARA, `m${i + 1}`));
  const c = b.buildChatState(view({ messages: { 1: many } }), "a", 1, hex(1));
  assert.equal(c.lines.length, b.CHAT_HISTORY);
  assert.equal(c.lines.at(-1).text, "m200");
});

test("an encrypted message shows what the main window shows: its decrypted text, or a note", () => {
  const sealedRow = (id) => msg(id, 3, MARA, "", { is_e2ee: true, sealed: [1, 2, 3] });
  const v = view({
    messages: { 3: [sealedRow(1), sealedRow(2), sealedRow(3)] },
    sealedDecrypts: { 1: { kind: "decrypted", content: "the plan" }, 2: { kind: "undecryptable", reason: "x" } },
  });
  const c = b.buildChatState(v, "a", 3, hex(1));
  assert.deepEqual(c.lines.map((l) => l.text), ["the plan", "(encrypted message, could not be decrypted)", "(encrypted message)"]);
});

test("a garble rule is not undone: other people's garbled words stay garbled, mine do not", () => {
  const mark = { mode: "bark", phrase: "", moderation: true };
  const v = view({ messages: { 1: [msg(1, 1, MARA, "hello world", { garble: mark }), msg(2, 1, ME, "hello world", { garble: mark })] } });
  const c = b.buildChatState(v, "a", 1, hex(1));
  assert.notEqual(c.lines[0].text, "hello world", "someone else's garbled message must not be shown as written");
  assert.equal(c.lines[1].text, "hello world");
});

test("attachments are named, not loaded", () => {
  const v = view({ messages: { 1: [msg(1, 1, MARA, "look", { attachments: [{ name: "cat.png" }] })] } });
  assert.equal(b.buildChatState(v, "a", 1, hex(1)).lines[0].text, "look [file: cat.png]");
});

test("requests from another window are checked", () => {
  const ok = { label: "chat-0badf00d-12", server_id: "s", channel_id: 12, last_id: 5 };
  assert.deepEqual(b.parseChatRef(ok), { label: "chat-0badf00d-12", serverId: "s", channelId: 12 });
  // A relay server id is a long link (address + id + certificate fingerprint): it must pass.
  assert.equal(b.parseChatRef({ ...ok, server_id: "farder://relay/45.61.136.29:4433/" + "a".repeat(32) + "/" + "b".repeat(64) }).channelId, 12);
  assert.equal(b.parseChatRead(ok).lastId, 5);
  for (const bad of [
    null, {}, { ...ok, label: "main" }, { ...ok, label: "chat-XYZ-1" }, { ...ok, label: "chat-0badf00d-" },
    { ...ok, server_id: "" }, { ...ok, server_id: "x".repeat(1025) }, { ...ok, channel_id: -1 }, { ...ok, channel_id: 1.5 }, { ...ok, channel_id: "1" },
  ]) assert.equal(b.parseChatRef(bad), null, JSON.stringify(bad));
  assert.equal(b.parseChatRead({ ...ok, last_id: -1 }), null);
  assert.equal(b.parseChatRead({ ...ok, last_id: "5" }), null);
});
