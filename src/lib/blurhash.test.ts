import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("blurhash", () => ({ decode: vi.fn(() => new Uint8ClampedArray(32 * 32 * 4)) }));

import { decode } from "blurhash";
import { blurhashToDataUrl } from "#lib/blurhash.js";
import { FLIP_ROW_LIMIT } from "#lib/motion.js";

const decodeMock = vi.mocked(decode);

// jsdom has no canvas: a 2D context that does what the placeholder needs.
beforeEach(() => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    createImageData: (width: number, height: number) => ({
      data: new Uint8ClampedArray(width * height * 4),
    }),
    putImageData: () => {},
  } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue("data:image/png;base64,");
});

describe("blurhash placeholders", () => {
  it("redraws the longest plain list from the cache", () => {
    // The queue panel's full list re-renders every cover on a track change.
    const hashes = Array.from({ length: FLIP_ROW_LIMIT }, (_, i) => `queue-${i}`);
    hashes.forEach(blurhashToDataUrl);
    decodeMock.mockClear();
    hashes.forEach(blurhashToDataUrl);
    expect(decodeMock).not.toHaveBeenCalled();
  });

  it("keeps covers in use while browsing cycles the rest out", () => {
    decodeMock.mockClear();
    for (let i = 0; i < 2_000; i++) {
      if (i % 100 === 0) blurhashToDataUrl("on-screen");
      blurhashToDataUrl(`browsed-${i}`);
    }
    // Decoded once, however many covers went through since.
    const onScreen = decodeMock.mock.calls.filter(([hash]) => hash === "on-screen");
    expect(onScreen).toHaveLength(1);
    // Still bounded: what was browsed long ago is gone.
    decodeMock.mockClear();
    blurhashToDataUrl("browsed-0");
    expect(decodeMock).toHaveBeenCalledTimes(1);
  });
});
