// Feeds PCM pushed from the Rust tap (via port.postMessage) into the audio
// graph so Butterchurn's analysers see the music. Output is muted upstream;
// this node only exists as an analysis signal source.
//
// Stereo: messages are interleaved Float32Arrays (L, R, L, R, …). Left and
// right go to output channels 0 and 1 — Butterchurn splits them into its own
// left and right analysers, so the waveforms show the real stereo picture.
//
// Ring buffer with a bounded latency: if the producer runs ahead (burst after
// a stall), we skip forward so visuals never lag the audible audio by much.
export class JellysicVisFeed extends AudioWorkletProcessor {
  constructor() {
    super();
    // Capacity and latency are in frames (one left + one right sample).
    this.capacity = sampleRate * 4;
    this.left = new Float32Array(this.capacity);
    this.right = new Float32Array(this.capacity);
    this.read = 0;
    this.write = 0;
    this.size = 0;
    this.targetLatency = Math.floor(sampleRate * 0.1);
    this.port.onmessage = (e) => this.push(e.data);
  }

  push(data) {
    const frames = data.length >> 1;
    for (let f = 0; f < frames; f++) {
      if (this.size >= this.capacity) {
        this.read = (this.read + 1) % this.capacity;
        this.size--;
      }
      this.left[this.write] = data[2 * f];
      this.right[this.write] = data[2 * f + 1];
      this.write = (this.write + 1) % this.capacity;
      this.size++;
    }
    const max = this.targetLatency * 3;
    if (this.size > max) {
      const drop = this.size - this.targetLatency;
      this.read = (this.read + drop) % this.capacity;
      this.size -= drop;
    }
  }

  process(inputs, outputs) {
    const out = outputs[0];
    const l = out[0];
    const r = out.length > 1 ? out[1] : null;
    for (let i = 0; i < l.length; i++) {
      if (this.size > 0) {
        const left = this.left[this.read];
        const right = this.right[this.read];
        // A mono output gets the mid signal rather than just the left side.
        l[i] = r ? left : (left + right) / 2;
        if (r) r[i] = right;
        this.read = (this.read + 1) % this.capacity;
        this.size--;
      } else {
        l[i] = 0;
        if (r) r[i] = 0;
      }
    }
    for (let c = 2; c < out.length; c++) out[c].set(l);
    return true;
  }
}

registerProcessor("jellysic-vis-feed", JellysicVisFeed);
