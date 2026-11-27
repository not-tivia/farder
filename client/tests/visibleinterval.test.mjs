import test from "node:test";
import assert from "node:assert/strict";
const v = await import(process.env.VISIBLE_JS);

// A fake window: manual clock, manual visibility.
function fakeEnv() {
  let hidden = false, next = 1, listeners = [];
  const timers = new Map();
  return {
    env: {
      isHidden: () => hidden,
      onVisibilityChange: (cb) => { listeners.push(cb); return () => { listeners = listeners.filter((l) => l !== cb); }; },
      setInterval: (cb, ms) => { const id = next++; timers.set(id, { cb, ms }); return id; },
      clearInterval: (id) => { timers.delete(id); },
    },
    tick: () => { for (const t of [...timers.values()]) t.cb(); },
    setHidden: (h) => { hidden = h; listeners.forEach((l) => l()); },
    activeTimers: () => timers.size,
    listenerCount: () => listeners.length,
  };
}

test("ticks while visible", () => {
  const f = fakeEnv(); let n = 0;
  const stop = v.visibleInterval(() => n++, 1000, f.env);
  f.tick(); f.tick();
  assert.equal(n, 2);
  stop();
});

test("while hidden there is no timer at all, so nothing can wake the machine", () => {
  const f = fakeEnv(); let n = 0;
  v.visibleInterval(() => n++, 1000, f.env);
  f.setHidden(true);
  assert.equal(f.activeTimers(), 0);
  f.tick();
  assert.equal(n, 0);
});

test("starting while already hidden never starts a timer", () => {
  const f = fakeEnv(); f.setHidden(true); let n = 0;
  v.visibleInterval(() => n++, 1000, f.env);
  assert.equal(f.activeTimers(), 0);
  assert.equal(n, 0);
});

test("becoming visible catches up exactly once and resumes", () => {
  const f = fakeEnv(); let n = 0;
  v.visibleInterval(() => n++, 1000, f.env);
  f.setHidden(true);
  f.setHidden(false);
  assert.equal(n, 1, "one catch-up call");
  assert.equal(f.activeTimers(), 1);
  f.tick();
  assert.equal(n, 2);
});

test("repeated visibility events never stack timers", () => {
  const f = fakeEnv(); let n = 0;
  v.visibleInterval(() => n++, 1000, f.env);
  f.setHidden(false); f.setHidden(false); f.setHidden(false);
  assert.equal(f.activeTimers(), 1);
  assert.equal(n, 0, "visible -> visible is not a catch-up");
});

test("stop clears the timer and the listener", () => {
  const f = fakeEnv();
  const stop = v.visibleInterval(() => {}, 1000, f.env);
  stop();
  assert.equal(f.activeTimers(), 0);
  assert.equal(f.listenerCount(), 0);
});
