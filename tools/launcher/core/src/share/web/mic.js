// x86Sim share page: the microphone's AudioWorklet (app.js micStart): the input resampled to 48 kHz mono, posted in
// 10 ms frames for the socket.
"use strict";
class M extends AudioWorkletProcessor {
  constructor() { super(); this.r = sampleRate / 48000; this.p = 0; this.prev = 0; this.out = new Float32Array(480); this.n = 0; }
  process(inputs) {
    const x = inputs[0] && inputs[0][0];
    if (!x) return true;
    // linear resampling to 48 kHz, posted in 10 ms frames
    let p = this.p;
    while (p < x.length) {
      const i = Math.floor(p), f = p - i, a = i === 0 ? this.prev : x[i - 1], b = x[i];
      this.out[this.n++] = a + (b - a) * f;
      if (this.n === 480) { this.port.postMessage(this.out.slice(0)); this.n = 0; }
      p += this.r;
    }
    this.p = p - x.length; this.prev = x[x.length - 1];
    return true;
  }
}
registerProcessor("x86sim-mic", M);
