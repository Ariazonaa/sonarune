/**
 * The level unmute returns to. A volume of 0 does not only come from the mute
 * button: the slider or the volume-down key can run it down to the bottom,
 * and Rust saves and restores the 0 across a restart. So the level is learned
 * from what the player reports, not just taken when mute is pressed: the last
 * volume that held for a moment (a drag or a held key passes through levels
 * nobody listened at), saved so it outlives the restart that brings the 0
 * back. Every window reads the same key.
 *
 * With per-device sound profiles (Settings → Audio) every output device keeps
 * its own volume, the 0 of a mute included: back on the headphones that were
 * muted, their 0 comes back, and unmute has to return to their level, not to
 * the one the speakers played at in between. So the levels are kept per
 * device, named by `audio:profile-applied` — the moment the volume changes
 * with the device. Until one is named, a level belongs to no device in
 * particular (""), which is all there is with profiles off.
 */
const STORAGE_KEY = "sonarune.unmuteVolume";

/** How long a level has to hold before unmute comes back to it. */
export const SETTLED_MS = 1000;

/** Unmute with no level known (muted since before this was kept, storage
 *  cleared): moderate, never the full gain a 0 would otherwise jump to. */
export const FALLBACK_UNMUTE_VOLUME = 0.25;

const audible = (volume: unknown): volume is number =>
  typeof volume === "number" && Number.isFinite(volume) && volume > 0 && volume <= 1;

interface Levels {
  /** The device played through last ("" while none was named). */
  device: string;
  /** The level to unmute to, by device. */
  levels: Map<string, number>;
}

/** What is stored, as far as it can be read: `{ device, levels }` as JSON. */
function parse(raw: string | null): Levels {
  const parsed: Levels = { device: "", levels: new Map() };
  if (raw === null) return parsed;
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== "object" || value === null) return parsed;
    const { device, levels } = value as { device?: unknown; levels?: unknown };
    if (typeof device === "string") parsed.device = device;
    if (typeof levels === "object" && levels !== null) {
      for (const [name, level] of Object.entries(levels)) {
        if (audible(level)) parsed.levels.set(name, level);
      }
    }
  } catch {
    // Not JSON: nothing usable in it.
  }
  return parsed;
}

export class UnmuteVolume {
  #storage: Storage | undefined;
  #device = "";
  #levels = new Map<string, number>();
  /** The volume the player reports right now, and since when. */
  #seen: { volume: number; since: number } | null = null;

  constructor(storage?: Storage) {
    this.#storage = storage;
    try {
      const stored = parse((storage ?? localStorage).getItem(STORAGE_KEY));
      this.#device = stored.device;
      this.#levels = stored.levels;
    } catch {
      // Storage unavailable: learned again from this session's volumes.
    }
  }

  /** The level to unmute to: this device's, else one learned before any
   *  device was named (the device playing then, most likely this one). */
  get level(): number {
    return this.#levels.get(this.#device) ?? this.#levels.get("") ?? FALLBACK_UNMUTE_VOLUME;
  }

  /** A volume the player reported, and when (a monotonic clock). */
  observe(volume: number, now: number): void {
    const seen = this.#seen;
    if (seen?.volume === volume) return;
    // The previous level played from `since` until now.
    if (seen && now - seen.since >= SETTLED_MS) this.remember(seen.volume);
    // The first one is what the player started with — restored from the last
    // session, so it has held already.
    this.#seen = { volume, since: seen ? now : -Infinity };
  }

  /** The output switched to `device`, and its sound profile set the volume it
   *  had there (the state that carries it follows). What played until now
   *  belongs to the device before; what comes next is where `device` was
   *  left, so it has held already, like a restored one. */
  switchDevice(device: string, now: number): void {
    const seen = this.#seen;
    if (seen && now - seen.since >= SETTLED_MS) this.remember(seen.volume);
    this.#seen = null;
    if (device === this.#device) return;
    this.#device = device;
    this.#save();
  }

  /** Mute pressed at `volume`: exactly the level to come back to. */
  remember(volume: number): void {
    if (!audible(volume) || this.#levels.get(this.#device) === volume) return;
    this.#levels.set(this.#device, volume);
    this.#save();
  }

  /** Store the device and its level. The other devices' levels stay as
   *  stored: another window may have learned one since this one read them. */
  #save(): void {
    try {
      const storage = this.#storage ?? localStorage;
      const { levels } = parse(storage.getItem(STORAGE_KEY));
      const level = this.#levels.get(this.#device);
      if (level !== undefined) levels.set(this.#device, level);
      storage.setItem(
        STORAGE_KEY,
        JSON.stringify({ device: this.#device, levels: Object.fromEntries(levels) }),
      );
    } catch {
      // Storage unavailable: the level still applies for this session.
    }
  }
}
