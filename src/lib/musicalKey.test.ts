import { describe, expect, it } from "vitest";
import { keyName } from "./musicalKey.js";

describe("keyName", () => {
  it("names keys the English way", () => {
    expect(keyName({ tonic: 9, minor: true }, "en")).toBe("A minor");
    expect(keyName({ tonic: 10, minor: false }, "en")).toBe("B♭ major");
  });

  it("names keys the German way, with H and lower-case minor", () => {
    expect(keyName({ tonic: 11, minor: false }, "de")).toBe("H-Dur");
    expect(keyName({ tonic: 10, minor: false }, "de")).toBe("B-Dur");
    expect(keyName({ tonic: 6, minor: true }, "de")).toBe("fis-Moll");
  });
});
