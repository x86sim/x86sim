// AudioWorklet: a processor that outputs a constant 0.25 (the check reads it back from an OfflineAudioContext)
class Const extends AudioWorkletProcessor { process(inputs, outputs) { for (const ch of outputs[0]) ch.fill(0.25); return true; } }
registerProcessor('webcov-const', Const);
