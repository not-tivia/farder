// Run with: bash scripts/run-unit-tests.sh   (builds src/lib/garble.ts with esbuild first)
import test from "node:test";
import assert from "node:assert/strict";
const g = await import(process.env.GARBLE_JS);

// Deterministic rng that walks a fixed list.
const seq = (...v) => { let i = 0; return () => v[i++ % v.length]; };

test("bark replaces every word and keeps spacing and punctuation", () => {
  const t = g.makeTransform("bark", "", seq(0));
  assert.equal(g.garbleText("hello there, friend!", t), "bark bark, bark!");
});

test("capitalisation shape is kept", () => {
  const t = g.makeTransform("bark", "", seq(0));
  assert.equal(g.garbleText("Hello THERE", t), "Bark BARK");
});

test("contractions are one word", () => {
  const t = g.makeTransform("custom", "x");
  assert.equal(g.garbleText("don't stop", t), "x x");
  assert.equal(g.garbleText("don’t", t), "x");
});

test("links are never garbled", () => {
  const t = g.makeTransform("custom", "x");
  assert.equal(g.garbleText("see https://farder.xyz/a?b=1 now", t), "x https://farder.xyz/a?b=1 x");
});

test("mentions, emoji tokens, angle tokens and code survive", () => {
  const t = g.makeTransform("custom", "x");
  assert.equal(g.garbleText("hi @Chris :smile: <#12> `code here`", t), "x @Chris :smile: <#12> `code here`");
  assert.equal(g.garbleText("a ```keep this\nas is``` b", t), "x ```keep this\nas is``` x");
});

test("a different casing of the same word is still replaced", () => {
  const t = g.makeTransform("custom", "x");
  assert.equal(g.garbleText("Ünïcode wörds 123", t), "x x x");
});

test("empty and symbol-only text is unchanged", () => {
  const t = g.makeTransform("bark");
  assert.equal(g.garbleText("", t), "");
  assert.equal(g.garbleText("?! ... :-)", t), "?! ... :-)");
});

test("scramble keeps length and case shape", () => {
  const t = g.makeTransform("scramble", "", seq(0.5));
  const out = g.garbleText("Word", t);
  assert.equal(out.length, 4);
  assert.ok(/^[A-Z][a-z]{3}$/.test(out), out);
});

test("shuffle keeps the same letters", () => {
  const t = g.makeTransform("shuffle", "", seq(0.1, 0.9, 0.4, 0.7));
  const out = g.garbleText("planet", t);
  assert.equal([...out].sort().join(""), [..."planet"].sort().join(""));
});

test("custom phrase is sanitised: control chars, newlines and length", () => {
  assert.equal(g.sanitizePhrase("hi\n\tthere\u0007"), "hithere");
  assert.equal(g.sanitizePhrase("a".repeat(100)).length, g.MAX_PHRASE);
  assert.equal(g.makeTransform("custom", "\n\n")("w"), "...");
});

test("an unknown mode throws instead of silently passing text through", () => {
  assert.throws(() => g.makeTransform("nope"));
});

test("display garbling is deterministic per message and differs between messages", () => {
  const mark = { mode: "bark", phrase: "" };
  const a1 = g.displayGarble("one two three four five six seven eight", mark, "hash-A");
  const a2 = g.displayGarble("one two three four five six seven eight", mark, "hash-A");
  const b = g.displayGarble("one two three four five six seven eight", mark, "hash-B");
  assert.equal(a1, a2, "same message, same drawing, every time and for every viewer");
  assert.notEqual(a1, "one two three four five six seven eight");
  assert.equal(a1.split(" ").length, 8, "spacing is kept");
  assert.notEqual(a1, b, "different messages draw differently (not one fixed output)");
});

test("an unknown mode from a newer client is shown as written, never dropped", () => {
  assert.equal(g.displayGarble("hello there", { mode: "from-the-future", phrase: "" }, "k"), "hello there");
});

test("the seeded generator stays inside [0,1) and the hash is stable", () => {
  const r = g.seededRng(g.hashSeed("x"));
  for (let i = 0; i < 1000; i++) { const v = r(); assert.ok(v >= 0 && v < 1); }
  assert.equal(g.hashSeed("abc"), g.hashSeed("abc"));
  assert.notEqual(g.hashSeed("abc"), g.hashSeed("abd"));
});

test("custom display uses the phrase from the mark", () => {
  assert.equal(g.displayGarble("a b", { mode: "custom", phrase: "boop" }, "k"), "boop boop");
});
