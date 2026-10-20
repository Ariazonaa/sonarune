import { describe, expect, it } from "vitest";
import { isLocalHost, normalizeServerUrl } from "#lib/serverUrl.js";

describe("normalizeServerUrl", () => {
  it("adds http:// to a host on this machine or the local network", () => {
    expect(normalizeServerUrl("localhost:8096")).toBe("http://localhost:8096");
    expect(normalizeServerUrl(" 10.0.0.5:8096 ")).toBe("http://10.0.0.5:8096");
    expect(normalizeServerUrl("nas:8096")).toBe("http://nas:8096");
    expect(normalizeServerUrl("jellyfin.local:8096")).toBe("http://jellyfin.local:8096");
    expect(normalizeServerUrl("[fd00::5]:8096")).toBe("http://[fd00::5]:8096");
  });

  it("keeps an address that names its scheme as typed", () => {
    expect(normalizeServerUrl("https://jellyfin.example.com")).toBe("https://jellyfin.example.com");
    expect(normalizeServerUrl("HTTP://NAS:8096/jellyfin/")).toBe("HTTP://NAS:8096/jellyfin/");
  });

  it("does not guess the scheme of a server across the internet", () => {
    expect(normalizeServerUrl("jellyfin.example.com")).toBeNull();
    expect(normalizeServerUrl("jellyfin.example.com:8096")).toBeNull();
    expect(normalizeServerUrl("203.0.113.7:8096")).toBeNull();
  });

  it("refuses what cannot be a server address", () => {
    expect(normalizeServerUrl("")).toBeNull();
    expect(normalizeServerUrl("   ")).toBeNull();
    expect(normalizeServerUrl("ftp://nas.local")).toBeNull();
    expect(normalizeServerUrl("http://")).toBeNull();
    expect(normalizeServerUrl("http://exa mple.com")).toBeNull();
  });
});

describe("isLocalHost", () => {
  it("knows the private ranges", () => {
    for (const host of ["127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.1", "169.254.1.1", "100.100.1.1"]) {
      expect(isLocalHost(host), host).toBe(true);
    }
    for (const host of ["172.32.0.1", "8.8.8.8", "100.128.0.1", "192.169.0.1"]) {
      expect(isLocalHost(host), host).toBe(false);
    }
  });

  it("knows local names and IPv6 ranges", () => {
    for (const host of ["localhost", "NAS", "music.home.arpa", "box.lan", "[::1]", "fe80::1", "fd12::1"]) {
      expect(isLocalHost(host), host).toBe(true);
    }
    for (const host of ["example.com", "2001:db8::1", "music.example.local.example.com"]) {
      expect(isLocalHost(host), host).toBe(false);
    }
  });
});
