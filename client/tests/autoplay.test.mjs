import test from "node:test";
import assert from "node:assert/strict";
const a = await import(process.env.AUTOPLAY_JS);

const calm = { dataSaverOn: false, reducedMotion: false };
const on = { enabled: true, inPopups: true, trusted: ["vk_aa", "vk_bb"] };

test("off by default, and a fresh settings object trusts nobody", () => {
  assert.equal(a.AUTOPLAY_DEFAULTS.enabled, false);
  assert.deepEqual(a.AUTOPLAY_DEFAULTS.trusted, []);
  assert.equal(a.shouldAutoplay(a.AUTOPLAY_DEFAULTS, "vk_aa", calm), false);
});

test("plays only for a trusted author when everything is switched on", () => {
  assert.equal(a.shouldAutoplay(on, "vk_aa", calm), true);
  assert.equal(a.shouldAutoplay(on, "vk_zz", calm), false, "an untrusted author never qualifies");
});

test("an unknown author never qualifies", () => {
  assert.equal(a.shouldAutoplay(on, null, calm), false);
  assert.equal(a.shouldAutoplay(on, undefined, calm), false);
  assert.equal(a.shouldAutoplay(on, "", calm), false);
});

test("Data Saver and reduced motion each veto it, even for a trusted author", () => {
  assert.equal(a.shouldAutoplay(on, "vk_aa", { ...calm, dataSaverOn: true }), false);
  assert.equal(a.shouldAutoplay(on, "vk_aa", { ...calm, reducedMotion: true }), false);
});

test("the master switch vetoes everything", () => {
  assert.equal(a.shouldAutoplay({ ...on, enabled: false }, "vk_aa", calm), false);
  assert.equal(a.shouldAutoplay({ ...on, enabled: false }, "vk_aa", calm, "popup"), false);
});

test("pop-ups have their own switch", () => {
  const noPopups = { ...on, inPopups: false };
  assert.equal(a.shouldAutoplay(noPopups, "vk_aa", calm, "chat"), true);
  assert.equal(a.shouldAutoplay(noPopups, "vk_aa", calm, "popup"), false);
});

test("trusting and untrusting someone is exact and idempotent", () => {
  let s = a.setTrusted(a.AUTOPLAY_DEFAULTS, "vk_cc", true);
  s = a.setTrusted(s, "vk_cc", true);
  assert.deepEqual(s.trusted, ["vk_cc"]);
  s = a.setTrusted(s, "vk_cc", false);
  assert.deepEqual(s.trusted, []);
  assert.deepEqual(a.AUTOPLAY_DEFAULTS.trusted, [], "the shared defaults object is never mutated");
});

test("a pop-up previews only direct media links over https", () => {
  assert.equal(a.firstDirectMediaUrl("look https://x.test/a/clip.mp4 wow"), "https://x.test/a/clip.mp4");
  assert.equal(a.firstDirectMediaUrl("(https://x.test/p.png)."), "https://x.test/p.png");
  assert.equal(a.firstDirectMediaUrl("https://x.test/v.webm?token=1 ok"), "https://x.test/v.webm?token=1");
  assert.equal(a.firstDirectMediaUrl("http://x.test/clip.mp4"), null, "plain http is never previewed");
  assert.equal(a.firstDirectMediaUrl("https://youtube.com/watch?v=abc"), null, "pages are not previewed");
  assert.equal(a.firstDirectMediaUrl("https://x.test/page"), null);
  assert.equal(a.firstDirectMediaUrl("no links here"), null);
  assert.equal(a.firstDirectMediaUrl("https://x.test/notes.mp4.exe"), null, "the extension must be the end");
});
