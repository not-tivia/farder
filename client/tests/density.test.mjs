import test from "node:test";
import assert from "node:assert/strict";
const d = await import(process.env.DENSITY_JS);

test("only the two spacious layouts fold repeated authors", () => {
  assert.deepEqual(d.DENSITIES.filter(d.foldsRepeatedAuthors), ["roomy", "normal"]);
});

test("unknown values are not densities", () => {
  for (const bad of [null, undefined, "", "Compact", "huge", 3, {}]) assert.equal(d.isDensity(bad), false);
  for (const ok of d.DENSITIES) assert.equal(d.isDensity(ok), true);
});

test("with storage unavailable the saved choice falls back to today's layout", () => {
  // node has no localStorage global, so getItem throws a ReferenceError: the same
  // path as a blocked or corrupt store.
  assert.equal(d.getDensity(), "normal");
  assert.equal(d.DEFAULT_DENSITY, "normal");
});

test("applying a density sets the data attribute the stylesheet keys on", () => {
  const root = { dataset: {} };
  d.applyDensity("irc", root);
  assert.equal(root.dataset.density, "irc");
});

test("every density has a label and a description for the settings screen", () => {
  for (const k of d.DENSITIES) {
    assert.ok(d.DENSITY_INFO[k].label.length > 0);
    assert.ok(d.DENSITY_INFO[k].description.length > 0);
  }
});

test("the stylesheet covers every non-default density and hard-codes no colour", () => {
  for (const k of ["roomy", "compact", "irc", "tiny"]) {
    assert.ok(d.DENSITY_CSS.includes(`html[data-density="${k}"]`), k);
  }
  // Theme-compatibility rule: colours come from variables only.
  assert.equal(/#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/.test(d.DENSITY_CSS), false);
});

test("the short clock is HH:MM with no day words", () => {
  const s = d.shortClock(1_700_000_000);
  assert.match(s, /^\d{2}:\d{2}$/);
});
