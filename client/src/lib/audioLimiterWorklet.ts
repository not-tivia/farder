// The loudness limiter for sound that plays inside the web view (videos and voice
// messages in chat). It is the same algorithm as src-tauri/src/audio_limit.rs:
// master volume first, then a hard ceiling with instant attack and a smooth
// 150 ms release, so a scream cannot be louder than the ceiling whatever the
// volume slider says.
//
// It is kept as SOURCE TEXT because an AudioWorklet runs in its own scope and is
// loaded from a URL. The same text is also run in the unit tests (with a stub
// `AudioWorkletProcessor`), so what is tested is what ships.

export const LIMITER_PROCESSOR_NAME = "farder-limiter";

export const LIMITER_WORKLET_SOURCE = `
class FarderLimiter extends AudioWorkletProcessor {
  constructor() {
    super();
    this.gain = 1;
    this.master = 1;
    this.ceil = 0; // linear ceiling; 0 = no ceiling
    this.coef = 1 - Math.exp(-1 / (sampleRate * 0.15));
    this.port.onmessage = (e) => {
      const d = e.data || {};
      if (typeof d.master === "number" && isFinite(d.master)) this.master = Math.min(1.5, Math.max(0, d.master));
      if (typeof d.ceiling === "number" && isFinite(d.ceiling)) this.ceil = d.ceiling > 0 && d.ceiling < 1 ? d.ceiling : 0;
    };
  }
  process(inputs, outputs) {
    const input = inputs[0];
    const output = outputs[0];
    if (!input || input.length === 0 || !output) return true;
    const channels = Math.min(input.length, output.length);
    const n = input[0].length;
    const ceil = this.ceil * 0.9999;
    for (let i = 0; i < n; i++) {
      // Channels are limited together, so a loud left side cannot be balanced by
      // a quiet right one and the stereo image does not wobble.
      let peak = 0;
      for (let c = 0; c < channels; c++) {
        const a = Math.abs(input[c][i] * this.master);
        if (a > peak) peak = a;
      }
      if (this.ceil > 0) {
        const released = this.gain + (1 - this.gain) * this.coef;
        const allowed = peak > 0 ? ceil / peak : Infinity;
        this.gain = Math.min(released, allowed, 1);
      } else {
        this.gain = 1;
      }
      for (let c = 0; c < channels; c++) {
        let v = input[c][i] * this.master * this.gain;
        if (this.ceil <= 0) v = Math.max(-1, Math.min(1, v));
        output[c][i] = v;
      }
    }
    return true;
  }
}
registerProcessor("${LIMITER_PROCESSOR_NAME}", FarderLimiter);
`;
