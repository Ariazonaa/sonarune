// Decoding and shaping of the PCM frames the Rust tap sends
// (src-tauri/src/player/tap.rs): u32-le sample rate, u32-le channel count,
// then f32-le samples interleaved L, R, L, R, …

const HEADER_BYTES = 8;
/** Anything above this is not a frame from our tap. */
const MAX_CHANNELS = 8;

interface PcmFrame {
  sampleRate: number;
  channels: number;
  /** Interleaved samples, whole frames only. */
  samples: Float32Array;
}

/** Parse one tap frame; null for anything malformed. */
export function decodeFrame(frame: ArrayBuffer): PcmFrame | null {
  if (frame.byteLength < HEADER_BYTES + 4) return null;
  const view = new DataView(frame);
  const sampleRate = view.getUint32(0, true);
  const channels = view.getUint32(4, true);
  if (channels < 1 || channels > MAX_CHANNELS) return null;
  const count = Math.floor((frame.byteLength - HEADER_BYTES) / 4);
  const whole = count - (count % channels);
  if (whole === 0) return null;
  // slice() copies into a fresh, 4-byte aligned buffer.
  const samples = new Float32Array(frame.slice(HEADER_BYTES, HEADER_BYTES + whole * 4));
  return { sampleRate, channels, samples };
}

/** The mid signal: every frame's channels averaged. */
export function toMono(samples: Float32Array, channels: number): Float32Array {
  if (channels === 1) return samples;
  const frames = Math.floor(samples.length / channels);
  const out = new Float32Array(frames);
  for (let f = 0; f < frames; f++) {
    let sum = 0;
    for (let c = 0; c < channels; c++) sum += samples[f * channels + c];
    out[f] = sum / channels;
  }
  return out;
}

/** Interleaved stereo for the worklet: mono is duplicated, anything wider
 *  keeps its first two channels (the tap only ever sends stereo). */
export function toStereo(samples: Float32Array, channels: number): Float32Array {
  if (channels === 2) return samples;
  const frames = Math.floor(samples.length / channels);
  const out = new Float32Array(frames * 2);
  for (let f = 0; f < frames; f++) {
    const left = samples[f * channels];
    out[f * 2] = left;
    out[f * 2 + 1] = channels > 1 ? samples[f * channels + 1] : left;
  }
  return out;
}

/** Linear resampling of interleaved PCM, each channel on its own — resampling
 *  the interleaved stream as one signal would blend left into right. */
export function resampleInterleaved(
  input: Float32Array,
  channels: number,
  from: number,
  to: number,
): Float32Array {
  const frames = Math.floor(input.length / channels);
  if (frames === 0 || from === to || from <= 0 || to <= 0) return input;
  const outFrames = Math.max(1, Math.round((frames * to) / from));
  const out = new Float32Array(outFrames * channels);
  const step = (frames - 1) / Math.max(1, outFrames - 1);
  for (let i = 0; i < outFrames; i++) {
    const pos = i * step;
    const at = Math.floor(pos);
    const frac = pos - at;
    const next = Math.min(at + 1, frames - 1);
    for (let c = 0; c < channels; c++) {
      const a = input[at * channels + c];
      const b = input[next * channels + c];
      out[i * channels + c] = a + (b - a) * frac;
    }
  }
  return out;
}
