import type { AudioDeviceSnapshot, OutputDevice, PlaybackSettings } from "#lib/types.js";

type OutputChoice = Pick<PlaybackSettings, "outputDevice" | "outputDeviceName">;

/** The name to show for the configured output device: as last seen, or what
 *  is stored (builds before 0.6.1 stored the name itself). */
export function configuredOutputName(playback: OutputChoice): string | null {
  return playback.outputDeviceName ?? playback.outputDevice;
}

/** The listed device the configured choice means: the one with its id, else
 *  the only one with its name as last seen — Windows lists a device under a
 *  new id now and then (a re-paired Bluetooth headset, a driver with one
 *  endpoint per sample rate), mostly with the name it had. A name two devices
 *  share finds neither. Mirrors `resolve_output` in player/mod.rs, except for
 *  a choice from before 0.6.1 (a bare endpoint name the list does not show). */
export function configuredOutput(
  playback: OutputChoice | null,
  devices: OutputDevice[],
): OutputDevice | null {
  const id = playback?.outputDevice;
  if (id == null) return null;
  const byId = devices.find((device) => device.id === id);
  if (byId) return byId;
  const name = playback?.outputDeviceName;
  const named = name == null ? [] : devices.filter((device) => device.name === name);
  return named.length === 1 ? named[0] : null;
}

/** A device is configured, and it is not among the devices listed now (see
 *  `configuredOutput`). */
export function configuredOutputMissing(
  playback: OutputChoice | null,
  devices: OutputDevice[],
): boolean {
  return playback?.outputDevice != null && configuredOutput(playback, devices) == null;
}

/** Playback stands in for the configured device on the system default. */
export function outputFallbackActive(
  playback: OutputChoice | null,
  snapshot: Pick<AudioDeviceSnapshot, "fallbackDevice">,
): boolean {
  return playback?.outputDevice != null && snapshot.fallbackDevice === playback.outputDevice;
}

/** What the output select stores for a pick ("" is the system default): the
 *  id, and the name to show while that device is away. */
export function pickOutput(id: string, devices: OutputDevice[]): OutputChoice {
  if (!id) return { outputDevice: null, outputDeviceName: null };
  return {
    outputDevice: id,
    outputDeviceName: devices.find((device) => device.id === id)?.name ?? null,
  };
}
