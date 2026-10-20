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
//
// A smaller backlog stays too: chunks arrive at the rate they are played, so
// whatever a main-thread stall (a preset switch compiling its shaders) piled
// up would otherwise delay the visuals for good. The lowest fill level over
// each half second is the part that was never needed; above a small jitter
// margin it is dropped.
//
// Each message is one tap chunk, and it lasts as long as it takes to be heard:
// ~43 ms at 48 kHz, but up to ~0.5 s for an 8 kHz track at half speed. Both
// trims work in whole chunks of that length, so a long one is never cut short
// and left to run dry long before the next one is due.
export class SonaruneVisFeed extends AudioWorkletProcessor {
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
    this.margin = Math.floor(sampleRate * 0.03);
    this.trimWindow = Math.floor(sampleRate * 0.5);
    this.lowest = Infinity;
    this.observed = 0;
    /** Frames in the latest message: one chunk, and so the time until the next. */
    this.chunk = 0;
    this.port.onmessage = (e) => this.push(e.data);
  }

  push(data) {
    const frames = data.length >> 1;
    this.chunk = frames;
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
    // A burst is cut back to the newest ~100 ms, or to the newest chunk when
    // that alone lasts longer: the next one only comes when it has played.
    const max = Math.max(this.targetLatency * 3, frames + this.margin);
    if (this.size > max) {
      const drop = this.size - Math.max(this.targetLatency, frames);
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
    this.trimBacklog(l.length);
    return true;
  }

  /** Drop the standing backlog: what stayed queued all through the window. */
  trimBacklog(rendered) {
    if (this.size < this.lowest) this.lowest = this.size;
    this.observed += rendered;
    // The fill is lowest just before a chunk arrives, so the window spans at
    // least two chunks; one that saw no arrival would take the rest of the
    // current chunk for backlog and drop what is still needed.
    if (this.observed < Math.max(this.trimWindow, 2 * this.chunk)) return;
    if (this.lowest > this.margin) {
      const drop = this.lowest - this.margin;
      this.read = (this.read + drop) % this.capacity;
      this.size -= drop;
    }
    this.lowest = Infinity;
    this.observed = 0;
  }
}

registerProcessor("sonarune-vis-feed", SonaruneVisFeed);
