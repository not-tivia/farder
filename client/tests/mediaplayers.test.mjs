import test from "node:test";
import assert from "node:assert/strict";
const m = await import(process.env.PLAYERS_JS);

const anchor = { x: 100, y: 50, w: 320, h: 180 };
const open = (state, input) => m.mediaPlayersReducer(state, { type: "open", input, anchor });
const vis = (state, hostId, visible) => m.mediaPlayersReducer(state, { type: "hostVisible", hostId, visible, anchor });

test("a clicked video floats when its card scrolls away and docks back", () => {
  let s = open(m.initialState, { kind: "video", src: "u", hostId: "h", float: false });
  s = vis(s, "h", false);
  assert.equal(s.players[0].state, "floating");
  s = vis(s, "h", true);
  assert.equal(s.players[0].state, "docked");
});

test("an autoplayed (quiet) video stays in its card when scrolled away", () => {
  let s = open(m.initialState, { kind: "video", src: "u", hostId: "h", float: false, quiet: true });
  assert.equal(s.players[0].quiet, true);
  s = vis(s, "h", false);
  assert.equal(s.players[0].state, "docked", "it pauses off-screen instead of floating over the chat");
});

test("a player is not quiet unless autoplay said so", () => {
  const s = open(m.initialState, { kind: "video", src: "u", hostId: "h" });
  assert.equal(s.players[0].quiet, false);
});

test("at the cap, a new autoplay replaces the oldest still-quiet autoplay instead of giving up", () => {
  // Seen in the two-app suite: after four autoplayed videos in a channel nothing could play again.
  let s = m.initialState;
  for (let i = 0; i < m.MAX_PLAYERS; i++) s = open(s, { kind: "video", src: "u" + i, hostId: "h" + i, float: false, quiet: true });
  assert.equal(s.players.length, m.MAX_PLAYERS);
  s = open(s, { kind: "video", src: "new", hostId: "hn", float: false, quiet: true });
  assert.equal(s.players.length, m.MAX_PLAYERS);
  assert.ok(s.players.some((p) => p.src === "new"), "the new one opened");
  assert.ok(!s.players.some((p) => p.src === "u0"), "the oldest quiet one made room");
});

test("a video you clicked or took over is never closed to make room", () => {
  let s = m.initialState;
  for (let i = 0; i < m.MAX_PLAYERS; i++) s = open(s, { kind: "video", src: "u" + i, hostId: "h" + i, float: false });
  const before = s.players.map((p) => p.id);
  s = open(s, { kind: "video", src: "new", hostId: "hn", float: false, quiet: true });
  assert.deepEqual(s.players.map((p) => p.id), before, "nothing was evicted and the autoplay did not open");
  // A quiet one that was taken over (quiet cleared) is not evictable either.
  let t = open(m.initialState, { kind: "video", src: "a", hostId: "ha", float: false, quiet: true });
  t = m.mediaPlayersReducer(t, { type: "update", id: t.players[0].id, patch: { quiet: false } });
  assert.equal(m.evictableQuiet(t.players), null);
});
