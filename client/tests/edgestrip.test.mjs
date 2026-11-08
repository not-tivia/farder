import test from "node:test";
import assert from "node:assert/strict";
const s = await import(process.env.EDGESTRIP_JS);

const note = (over = {}) => ({
  serverId: "s1", channelId: 1, mode: "full", sender: "Mara", channel: "general", isDm: false,
  title: "Mara in #general", body: "hello", reply: { server_id: "s1", channel_id: 1 }, ...over,
});

test("a conversation's chip counts up and moves to the front", () => {
  const m = new s.StripModel();
  m.note(note());
  m.note(note({ channelId: 2, channel: "lab" }));
  m.note(note());
  const st = m.state();
  assert.deepEqual(st.chips.map((c) => [c.label, c.count]), [["#general", 2], ["#lab", 1]]);
});

test("a direct message is labelled with the person, a channel with its name", () => {
  assert.equal(s.chipLabel("full", { sender: "Mara", channel: null, isDm: true }), "Mara");
  assert.equal(s.chipLabel("full", { sender: "Mara", channel: "general", isDm: false }), "#general");
  assert.equal(s.chipLabel("full", { sender: null, channel: null, isDm: true }), "Direct message");
});

test("sender-only mode never shows a channel name", () => {
  assert.equal(s.chipLabel("sender", { sender: "Mara", channel: "secret-plans", isDm: false }), "Mara");
  assert.equal(s.chipLabel("sender", { sender: null, channel: "secret-plans", isDm: false }), "New message");
});

test("hidden mode folds every conversation into one anonymous chip", () => {
  const m = new s.StripModel();
  m.note(note({ mode: "hidden" }));
  m.note(note({ mode: "hidden", serverId: "s2", channelId: 9, sender: "Dev" }));
  const st = m.state();
  assert.equal(st.chips.length, 1);
  assert.deepEqual(st.chips[0], { key: "all", label: "New messages", count: 2, server_id: null, channel_id: null });
  assert.equal(JSON.stringify(st.chips).includes("Mara"), false);
  assert.equal(JSON.stringify(st.chips).includes("general"), false);
});

test("only the most recent conversations are kept", () => {
  const m = new s.StripModel();
  for (let i = 0; i < s.MAX_CHIPS + 3; i++) m.note(note({ channelId: i, channel: `c${i}` }));
  const st = m.state();
  assert.equal(st.chips.length, s.MAX_CHIPS);
  assert.equal(st.chips[0].label, `#c${s.MAX_CHIPS + 2}`); // newest first
  assert.equal(st.chips.some((c) => c.label === "#c0"), false); // oldest dropped
});

test("each message gets a new preview id", () => {
  const m = new s.StripModel();
  m.note(note());
  const a = m.state().preview.id;
  m.note(note());
  assert.ok(m.state().preview.id > a);
});

test("looking at Farder clears everything", () => {
  const m = new s.StripModel();
  m.note(note());
  m.clear();
  assert.deepEqual(m.state(), { chips: [], preview: null });
});

test("replying to a conversation resolves only that chip", () => {
  const m = new s.StripModel();
  m.note(note());
  m.note(note({ channelId: 2, channel: "lab" }));
  m.resolve("s1", 1);
  assert.deepEqual(m.state().chips.map((c) => c.label), ["#lab"]);
  assert.notEqual(m.state().preview, null);
  m.resolve("s1", 2);
  assert.equal(m.state().preview, null);
});

test("an open request from the strip window is checked, not trusted", () => {
  assert.deepEqual(s.parseOpen({ server_id: "s1", channel_id: 4 }), { serverId: "s1", channelId: 4 });
  assert.deepEqual(s.parseOpen({ server_id: "s1" }), { serverId: "s1", channelId: null });
  for (const bad of [null, {}, { server_id: "" }, { server_id: 5 }, { server_id: "s", channel_id: -1 }, { server_id: "s", channel_id: 1.5 }, { server_id: "s", channel_id: "1" }, { server_id: "x".repeat(200) }]) {
    assert.equal(s.parseOpen(bad), null, JSON.stringify(bad));
  }
});
