import { beforeEach, describe, expect, it } from "vitest";
import { FALLBACK_UNMUTE_VOLUME, SETTLED_MS, UnmuteVolume } from "#lib/unmuteVolume.js";

const KEY = "sonarune.unmuteVolume";
const stored = () => JSON.parse(localStorage.getItem(KEY) ?? "null");

beforeEach(() => localStorage.clear());

describe("unmute level", () => {
  it("comes back to the level mute was pressed at, across a restart", () => {
    const before = new UnmuteVolume();
    before.observe(0.2, 0);
    before.remember(0.2);
    before.observe(0, 50);
    expect(stored()).toEqual({ device: "", levels: { "": 0.2 } });

    // Rust restores the muted 0; the level comes from storage.
    const after = new UnmuteVolume();
    after.observe(0, 0);
    expect(after.level).toBe(0.2);
  });

  it("skips the levels a slider drag down to zero passes through", () => {
    const unmute = new UnmuteVolume();
    unmute.observe(0.3, 0); // what the player started with
    unmute.observe(0.12, 60_000);
    unmute.observe(0.02, 60_040);
    unmute.observe(0.001, 60_080);
    unmute.observe(0, 60_120);
    expect(unmute.level).toBe(0.3);
  });

  it("learns a level once it has held, without mute being pressed", () => {
    const unmute = new UnmuteVolume();
    unmute.observe(0.3, 0);
    unmute.observe(0.6, 1_000); // dragged up; 0.3 had held since the start
    expect(unmute.level).toBe(0.3);
    unmute.observe(0.55, 1_000 + SETTLED_MS); // 0.6 held long enough
    unmute.observe(0, 1_000 + SETTLED_MS + 30);
    expect(unmute.level).toBe(0.6);
  });

  it("keeps mute's exact level over a passing one", () => {
    const unmute = new UnmuteVolume();
    unmute.observe(0.4, 0);
    unmute.observe(0.7, 10_000);
    unmute.remember(0.7); // M right after the drag
    unmute.observe(0, 10_100);
    expect(unmute.level).toBe(0.7);
  });

  it("falls back to a moderate level, not full volume, when nothing is known", () => {
    const unmute = new UnmuteVolume();
    unmute.observe(0, 0);
    expect(unmute.level).toBe(FALLBACK_UNMUTE_VOLUME);
    expect(FALLBACK_UNMUTE_VOLUME).toBeLessThan(0.5);
  });

  it("ignores stored values that are no audible level", () => {
    for (const raw of [
      "0.4", // not the stored shape
      "loud",
      "null",
      '{"device":7,"levels":{"":0,"a":-1,"b":7,"c":"0.3"}}',
    ]) {
      localStorage.setItem(KEY, raw);
      expect(new UnmuteVolume().level).toBe(FALLBACK_UNMUTE_VOLUME);
    }
  });

  it("never throws when storage is unavailable", () => {
    const denied = () => {
      throw new Error("denied");
    };
    const broken = { getItem: denied, setItem: denied, removeItem: denied } as unknown as Storage;
    const unmute = new UnmuteVolume(broken);
    expect(() => unmute.remember(0.5)).not.toThrow();
    expect(() => unmute.switchDevice("Speakers", 0)).not.toThrow();
    expect(unmute.level).toBe(0.5);
  });
});

// Per-device sound profiles: Rust restores each device's own volume when the
// output switches to it, the 0 of a mute included.
describe("unmute level per output device", () => {
  it("returns to the headphones' level, not the one the speakers played at", () => {
    const unmute = new UnmuteVolume();
    unmute.switchDevice("Headphones", 0);
    unmute.observe(0.2, 0);
    unmute.remember(0.2); // M on the headphones
    unmute.observe(0, 60_000);

    // The speakers' profile brings 0.8, listened to for minutes.
    unmute.switchDevice("Speakers", 120_000);
    unmute.observe(0.8, 120_000);
    // Back on the headphones, whose profile brings back their 0.
    unmute.switchDevice("Headphones", 400_000);
    unmute.observe(0, 400_000);
    expect(unmute.level).toBe(0.2);
  });

  it("is not thrown off by turning the speakers up or down in between", () => {
    const unmute = new UnmuteVolume();
    unmute.observe(0.2, 0); // the headphones, before any switch named them
    unmute.remember(0.2);
    unmute.observe(0, 1_000);

    unmute.switchDevice("Speakers", 120_000);
    unmute.observe(0.8, 120_000);
    unmute.observe(0.6, 300_000); // 0.8 held for minutes on the speakers
    unmute.observe(0.65, 400_000);
    unmute.switchDevice("Headphones", 500_000);
    unmute.observe(0, 500_000);
    expect(unmute.level).toBe(0.2);

    // And the speakers keep theirs for when they come back muted.
    unmute.switchDevice("Speakers", 600_000);
    unmute.observe(0, 600_000);
    expect(unmute.level).toBe(0.65);
  });

  it("knows the device it was left on after a restart", () => {
    const before = new UnmuteVolume();
    before.switchDevice("Speakers", 0);
    before.observe(0.7, 0);
    before.switchDevice("Headphones", 60_000);
    before.observe(0.15, 60_000);
    before.remember(0.15);
    before.observe(0, 70_000);
    expect(stored()).toEqual({ device: "Headphones", levels: { Speakers: 0.7, Headphones: 0.15 } });

    const after = new UnmuteVolume();
    after.observe(0, 0);
    expect(after.level).toBe(0.15);
  });

  it("keeps a level another window stored for a different device", () => {
    const main = new UnmuteVolume();
    const mini = new UnmuteVolume(); // read the storage before main wrote it
    main.switchDevice("Speakers", 0);
    main.remember(0.7);
    mini.switchDevice("Headphones", 0);
    mini.remember(0.15);
    expect(stored().levels).toEqual({ Speakers: 0.7, Headphones: 0.15 });
  });
});
