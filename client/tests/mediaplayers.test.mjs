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
