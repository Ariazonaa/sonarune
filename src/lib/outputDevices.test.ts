import { describe, expect, it } from "vitest";
import {
  configuredOutput,
  configuredOutputMissing,
  configuredOutputName,
  outputFallbackActive,
  pickOutput,
} from "./outputDevices";
import type { OutputDevice } from "./types";

// Windows calls the onboard output and a USB headset both "Speakers".
const onboard: OutputDevice = { id: "wasapi:{onboard}", name: "Speakers" };
const usb: OutputDevice = { id: "wasapi:{usb}", name: "Speakers" };
const devices = [onboard, usb];

describe("pickOutput", () => {
  it("keeps two devices of one name apart", () => {
    expect(pickOutput(usb.id, devices)).toEqual({ outputDevice: usb.id, outputDeviceName: "Speakers" });
    expect(pickOutput(onboard.id, devices).outputDevice).toBe(onboard.id);
  });

  it("stores nothing for the system default", () => {
    expect(pickOutput("", devices)).toEqual({ outputDevice: null, outputDeviceName: null });
  });
});

describe("configuredOutputMissing", () => {
  it("looks the configured device up by id", () => {
    expect(configuredOutputMissing({ outputDevice: usb.id, outputDeviceName: null }, devices)).toBe(
      false,
    );
    // The other "Speakers" being present does not make this one present.
    expect(configuredOutputMissing({ outputDevice: usb.id, outputDeviceName: null }, [onboard])).toBe(
      true,
    );
    expect(configuredOutputMissing({ outputDevice: null, outputDeviceName: null }, [])).toBe(false);
  });
});

describe("configuredOutput", () => {
  // Windows made a new endpoint for the same device (a Focusrite interface
  // switched to 48 kHz, a re-paired Bluetooth headset): new id, same name.
  const focusrite = "Speakers (Focusrite USB Audio)";
  const at48k: OutputDevice = { id: "wasapi:{48k}", name: focusrite };
  const realtek: OutputDevice = { id: "wasapi:{realtek}", name: "Speakers (Realtek(R) Audio)" };
  const chosenAt44k = { outputDevice: "wasapi:{44k}", outputDeviceName: focusrite };

  it("finds the device under a new id by its name", () => {
    expect(configuredOutput(chosenAt44k, [realtek, at48k])).toEqual(at48k);
    expect(configuredOutputMissing(chosenAt44k, [realtek, at48k])).toBe(false);
    expect(configuredOutputMissing(chosenAt44k, [realtek])).toBe(true);
  });

  it("prefers the id, and a name two devices share finds neither", () => {
    expect(configuredOutput({ outputDevice: realtek.id, outputDeviceName: focusrite }, [realtek, at48k]))
      .toEqual(realtek);
    const twin: OutputDevice = { id: "wasapi:{twin}", name: focusrite };
    expect(configuredOutput(chosenAt44k, [at48k, twin])).toBeNull();
  });

  it("needs the name as last seen", () => {
    expect(configuredOutput({ outputDevice: "wasapi:{44k}", outputDeviceName: null }, [at48k])).toBeNull();
    expect(configuredOutput({ outputDevice: null, outputDeviceName: focusrite }, [at48k])).toBeNull();
  });
});

describe("configuredOutputName", () => {
  it("shows the name as last seen, else what an older build stored", () => {
    expect(configuredOutputName({ outputDevice: usb.id, outputDeviceName: "Headset" })).toBe(
      "Headset",
    );
    expect(configuredOutputName({ outputDevice: "Speakers", outputDeviceName: null })).toBe(
      "Speakers",
    );
  });
});

describe("outputFallbackActive", () => {
  it("is on only for the configured device", () => {
    const playback = { outputDevice: usb.id, outputDeviceName: "Speakers" };
    expect(outputFallbackActive(playback, { fallbackDevice: usb.id })).toBe(true);
    expect(outputFallbackActive(playback, { fallbackDevice: null })).toBe(false);
    expect(
      outputFallbackActive({ outputDevice: null, outputDeviceName: null }, { fallbackDevice: null }),
    ).toBe(false);
  });
});
