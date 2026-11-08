// Runs the SHIPPED worklet source against the same properties the Rust limiter is tested for.
import test from "node:test";
import assert from "node:assert/strict";
const w = await import(process.env.LIMITER_JS);

const SR = 48000;
const dbToLin = (db) => Math.pow(10, db / 20);

function makeProcessor({ master = 1, ceilingDb = null } = {}) {
  let Cls = null;
  class AudioWorkletProcessor { constructor() { this.port = {}; } }
  new Function("AudioWorkletProcessor", "registerProcessor", "sampleRate", w.LIMITER_WORKLET_SOURCE)(
    AudioWorkletProcessor, (_name, c) => { Cls = c; }, SR);
  const p = new Cls();
  p.port.onmessage({ data: { master, ceiling: ceilingDb === null ? 0 : dbToLin(ceilingDb) } });
  return p;
}
// Process a mono signal in 128-sample render quanta, like the browser does.
function run(p, signal) {
  const out = new Float32Array(signal.length);
  for (let i = 0; i < signal.length; i += 128) {
    const inp = signal.slice(i, i + 128), o = new Float32Array(inp.length);
    p.process([[inp]], [[o]]);
    out.set(o, i);
  }
  return out;
}
const peak = (a) => a.reduce((m, v) => Math.max(m, Math.abs(v)), 0);
const sine = (amp, hz, n) => Float32Array.from({ length: n }, (_, i) => amp * Math.sin(2 * Math.PI * hz * i / SR));

test("nothing ever exceeds the ceiling, whatever is thrown at it", () => {
  for (const db of [-40, -24, -12, -6, -3]) {
    const ceil = dbToLin(db);
    const cases = {
      sine: sine(1, 440, SR),
      square: Float32Array.from({ length: SR }, (_, i) => (Math.floor(i / 50) % 2 === 0 ? 1 : -1)),
      step: Float32Array.from({ length: 9600 }, (_, i) => (i < 4800 ? 0 : 8)),
      noise: (() => { let x = 12345; return Float32Array.from({ length: SR }, () => { x = (Math.imul(x, 1664525) + 1013904223) >>> 0; return ((x >>> 8) / 8388608 - 1) * 6; }); })(),
    };
    for (const [name, sig] of Object.entries(cases)) {
      const out = run(makeProcessor({ master: 1.5, ceilingDb: db }), sig);
      assert.ok(peak(out) <= ceil, `${name} at ${db} dB peaked at ${peak(out)} > ${ceil}`);
    }
  }
});

test("quiet sound is untouched", () => {
  const q = sine(0.01, 300, 9600);
  const out = run(makeProcessor({ ceilingDb: -20 }), q);
  for (let i = 0; i < q.length; i++) assert.ok(Math.abs(q[i] - out[i]) < 1e-6);
});

test("master volume applies before the ceiling, so raising it cannot beat it", () => {
  const talk = sine(0.2, 200, 9600), ceil = dbToLin(-12);
  assert.ok(peak(run(makeProcessor({ master: 1.5, ceilingDb: -12 }), talk)) <= ceil);
  assert.ok(Math.abs(peak(run(makeProcessor({ master: 0.5, ceilingDb: -12 }), talk)) - 0.1) < 0.002);
});

test("it recovers after a scream, but not instantly", () => {
  const ceil = dbToLin(-12);
  const p = makeProcessor({ ceilingDb: -12 });
  run(p, new Float32Array(960).fill(1));
  const soon = run(p, sine(0.02, 250, 480));
  assert.ok(peak(soon) < 0.02 * 0.9, "still held down right after the scream");
  const later = run(p, sine(0.02, 250, SR));
  assert.ok(peak(later.slice(-960)) / 0.02 > 0.99, "back to full level a second later");
});

test("with no ceiling only the master applies and nothing exceeds full scale", () => {
  assert.ok(Math.abs(run(makeProcessor({ master: 0.5 }), new Float32Array(256).fill(0.5))[10] - 0.25) < 1e-6);
  assert.ok(peak(run(makeProcessor({ master: 1.5 }), new Float32Array(256).fill(0.9))) <= 1);
  assert.equal(peak(run(makeProcessor({ master: 0, ceilingDb: -6 }), new Float32Array(256).fill(0.9))), 0);
});

test("a stereo signal is limited together and neither side exceeds the ceiling", () => {
  const p = makeProcessor({ ceilingDb: -12 }), ceil = dbToLin(-12);
  const L = Float32Array.from({ length: 128 }, () => 1), R = Float32Array.from({ length: 128 }, () => 0.01);
  const oL = new Float32Array(128), oR = new Float32Array(128);
  p.process([[L, R]], [[oL, oR]]);
  assert.ok(peak(oL) <= ceil && peak(oR) <= ceil);
  assert.ok(Math.abs(oR[5] / oL[5] - 0.01) < 1e-3, "the balance between the sides is preserved");
});

test("hostile or garbage settings are ignored, not applied", () => {
  const p = makeProcessor({ master: 1 });
  p.port.onmessage({ data: { master: NaN, ceiling: Infinity } });
  p.port.onmessage({ data: { master: "loud", ceiling: -1 } });
  assert.equal(p.master, 1);
  assert.equal(p.ceil, 0, "a ceiling outside (0,1) means off");
  p.port.onmessage({ data: { master: 99 } });
  assert.equal(p.master, 1.5);
});
