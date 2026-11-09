import test from "node:test";
import assert from "node:assert/strict";
const l = await import(process.env.LAYOUT_JS);

test("only the two known layouts are accepted", () => {
  assert.deepEqual([...l.LAYOUTS], ["full", "small"]);
  for (const bad of [null, undefined, "", "Small", "tiny", 1, {}]) assert.equal(l.isLayout(bad), false);
  assert.equal(l.isLayout("full"), true);
  assert.equal(l.isLayout("small"), true);
});

test("with storage unavailable the app stays in the full layout", () => {
  assert.equal(l.getLayout(), "full"); // node has no localStorage: same path as a blocked store
  assert.equal(l.DEFAULT_LAYOUT, "full");
});

test("applying a layout sets the attribute the stylesheet keys on", () => {
  const root = { dataset: {} };
  l.applyLayout("small", root);
  assert.equal(root.dataset.layout, "small");
});

test("the stylesheet hides the wide-layout parts only in the small layout, and keeps the new list", () => {
  const css = l.LAYOUT_CSS;
  for (const sel of [".server-strip", ".member-sidebar", ".dm-panel"]) assert.ok(css.includes(sel), sel);
  // The compact list is itself a `.channel-sidebar`, so the hide rule must exclude it.
  assert.ok(css.includes(".channel-sidebar:not(.compact-list)"));
  // Everything is scoped to the small layout: nothing may change the full layout.
  for (const line of css.split("\n").filter((x) => x.includes("{") || x.includes(","))) {
    if (line.trim()) assert.ok(line.includes('html[data-layout="small"]'), line);
  }
  // Theme-compatibility rule: no literal colours.
  assert.equal(/#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/.test(css), false);
});

test("each layout has a label and a description", () => {
  for (const k of l.LAYOUTS) {
    assert.ok(l.LAYOUT_INFO[k].label.length > 0);
    assert.ok(l.LAYOUT_INFO[k].description.length > 0);
  }
});
