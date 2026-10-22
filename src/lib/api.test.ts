import { beforeEach, describe, expect, it, vi } from "vitest";

// The whole point of api.ts is to map typed calls onto Tauri `invoke` with
// camelCase args. Mock the command bridge and assert the wire contract.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { api, formatDuration } from "#lib/api.js";

const invokeMock = vi.mocked(invoke);

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined as never);
});

describe("api invoke contract", () => {
  it("connect forwards camelCase args to the connect command", async () => {
    const input = {
      serverUrl: "https://s",
      username: "u",
      password: "p",
      acceptInvalidCerts: false,
    };
    await api.connect(input);
    expect(invokeMock).toHaveBeenCalledWith("connect", input);
  });

  it("maps positional args into the named payload", async () => {
    await api.getAlbum("album-42");
    expect(invokeMock).toHaveBeenCalledWith("get_album", { albumId: "album-42" });

    await api.playAlbum("album-42", 3);
    expect(invokeMock).toHaveBeenCalledWith("play_album", { albumId: "album-42", startIndex: 3 });
  });

  it("re-reads tracks by id", async () => {
    await api.getTracks(["t-1", "t-2"]);
    expect(invokeMock).toHaveBeenCalledWith("get_tracks", { trackIds: ["t-1", "t-2"] });
  });

  it("addresses a pinned certificate by server and fingerprint", async () => {
    await api.listTrustedCertificates();
    expect(invokeMock).toHaveBeenCalledWith("list_trusted_certificates");

    await api.forgetTrustedCertificate("https://music.home", "AB:CD");
    expect(invokeMock).toHaveBeenCalledWith("forget_trusted_certificate", {
      serverUrl: "https://music.home",
      fingerprint: "AB:CD",
    });
  });

  it("re-pins the running session's certificate by fingerprint alone", async () => {
    await api.trustSessionCertificate("AB:CD");
    expect(invokeMock).toHaveBeenCalledWith("trust_session_certificate", {
      fingerprint: "AB:CD",
    });

    const check = { online: false, untrustedCertificate: "AB:CD" };
    invokeMock.mockResolvedValueOnce(check as never);
    await expect(api.checkServer()).resolves.toEqual(check);
    expect(invokeMock).toHaveBeenCalledWith("check_server");
  });

  it("asks for a track's waveform by item id", async () => {
    await api.getWaveform("track-7");
    expect(invokeMock).toHaveBeenCalledWith("get_waveform", { itemId: "track-7" });
  });

  it("fetches a pinned Weekly preset by file name", async () => {
    const file = `${"a".repeat(32)}.json`;
    invokeMock.mockResolvedValueOnce('{"baseVals":{}}' as never);
    await expect(api.fetchWeeklyPreset(file)).resolves.toBe('{"baseVals":{}}');
    expect(invokeMock).toHaveBeenCalledWith("fetch_weekly_preset", { file });
  });

  it("applies the default start index", async () => {
    await api.playAlbum("album-1");
    expect(invokeMock).toHaveBeenCalledWith("play_album", { albumId: "album-1", startIndex: 0 });
  });

  it("names a queue entry by its entry id as well as its item", async () => {
    await api.queueRemove(3, "track-1", "entry-9");
    expect(invokeMock).toHaveBeenCalledWith("queue_remove", {
      index: 3,
      itemId: "track-1",
      entryId: "entry-9",
    });
    await api.playerSetStopAfter("album", "track-1", "entry-9");
    expect(invokeMock).toHaveBeenCalledWith("player_set_stop_after", {
      mode: "album",
      itemId: "track-1",
      entryId: "entry-9",
    });
    // Nothing named: the stop goes on what plays now.
    await api.playerSetStopAfter("track");
    expect(invokeMock).toHaveBeenCalledWith("player_set_stop_after", {
      mode: "track",
      itemId: null,
      entryId: null,
    });
  });

  it("returns whatever the command resolves to", async () => {
    invokeMock.mockResolvedValueOnce({ id: "x" } as never);
    await expect(api.getArtist("x")).resolves.toEqual({ id: "x" });
    expect(invokeMock).toHaveBeenCalledWith("get_artist", { artistId: "x" });
  });

  it("reads back what the window may have missed, without arguments", async () => {
    invokeMock.mockResolvedValueOnce(7 as never);
    await expect(api.checkLibraryNow()).resolves.toBe(7);
    expect(invokeMock).toHaveBeenCalledWith("check_library_now");

    await api.getKnownUpdate();
    expect(invokeMock).toHaveBeenCalledWith("get_known_update");

    await api.getDatabaseRecovery();
    expect(invokeMock).toHaveBeenCalledWith("get_database_recovery");
  });
});

describe("formatDuration", () => {
  it("renders m:ss below an hour", () => {
    expect(formatDuration(5_000)).toBe("0:05");
    expect(formatDuration(59_499)).toBe("0:59");
    expect(formatDuration(59_500)).toBe("1:00");
    expect(formatDuration(3_599_000)).toBe("59:59");
  });

  it("renders h:mm:ss from one hour on", () => {
    expect(formatDuration(3_600_000)).toBe("1:00:00");
    expect(formatDuration(3_599_600)).toBe("1:00:00"); // rounds up into the hour
    expect(formatDuration((2 * 3600 + 5 * 60 + 30) * 1000)).toBe("2:05:30");
    expect(formatDuration(125 * 3600 * 1000)).toBe("125:00:00");
  });

  it("reads zero, negative and non-finite input as 0:00", () => {
    expect(formatDuration(0)).toBe("0:00");
    expect(formatDuration(-4_000)).toBe("0:00");
    expect(formatDuration(Number.NaN)).toBe("0:00");
    expect(formatDuration(Number.POSITIVE_INFINITY)).toBe("0:00");
  });
});
