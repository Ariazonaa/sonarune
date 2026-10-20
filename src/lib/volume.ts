// The volume scale shared by the player bar's slider and the volume
// shortcuts, so a key press and a drag move along the same curve.

/**
 * Perceptual taper: loudness is roughly logarithmic, so a linear slider would
 * cram the useful quiet and mid range into a tiny stretch. The slider position
 * runs through a power curve instead — more travel, finer control, where it
 * matters. The stored volume stays the linear gain Rust applies.
 */
const TAPER = 2.5;

const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/** Slider position (0–1) for a linear gain. */
export const volumeToPosition = (volume: number) => Math.pow(clamp01(volume), 1 / TAPER);

/** Linear gain for a slider position (0–1). */
export const positionToVolume = (position: number) => Math.pow(clamp01(position), TAPER);

/**
 * The gain `delta` of slider travel away from `volume`. A step in gain would
 * be about 1 % of the slider near the top and jump to silence within two
 * presses at a typical desktop level; a step along the slider is the same
 * distance everywhere.
 */
export function stepVolume(volume: number, delta: number): number {
  return positionToVolume(volumeToPosition(volume) + delta);
}
