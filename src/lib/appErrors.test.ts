import { afterEach, describe, expect, it } from "vitest";
import { m } from "#lib/paraglide/messages.js";
import { getLocale, setLocale } from "#lib/paraglide/runtime.js";
import {
  WORDED_CODES,
  errorCause,
  errorDetail,
  errorMessage,
  errorReport,
  isNotFound,
} from "./appErrors";

// The Rust side of the contract, as text (Vite glob: no fs access).
const rust = Object.values(
  import.meta.glob("/src-tauri/src/error.rs", { query: "?raw", import: "default", eager: true }),
)[0] as string;

/** A report as `a_command_error_arrives_as_code_and_detail` pins it. */
const albumEmpty = { code: "albumEmpty", detail: "album has no tracks" };

describe("errorCause", () => {
  const locale = getLocale();
  afterEach(() => setLocale(locale, { reload: false }));

  it("words a code in the user's language", () => {
    setLocale("de", { reload: false });
    expect(errorCause(albumEmpty)).toBe("das Album hat keine Titel");
    expect(errorMessage(albumEmpty)).toBe("Etwas ist schiefgelaufen: das Album hat keine Titel");
    expect(errorCause({ code: "playerGone", detail: "audio error: player thread is gone" })).toBe(
      m.error_player_gone(),
    );
    setLocale("en", { reload: false });
    expect(errorCause(albumEmpty)).toBe("the album has no tracks");
  });

  it("says a server error's status, unless the code has words of its own", () => {
    const busy = { code: "server", status: 503, detail: "server returned 503: busy" };
    expect(errorCause(busy)).toBe(m.player_error_cause_server({ status: "503" }));
    const wrong = { code: "wrongCredentials", status: 401, detail: "server returned 401: wrong" };
    expect(errorCause(wrong)).toBe(m.error_wrong_credentials());
  });

  it("adds the reason where it is the point", () => {
    setLocale("de", { reload: false });
    // What the server refused, in its words.
    const refused = {
      code: "server",
      status: 400,
      reason: "Name is too long",
      detail: "server returned 400: Name is too long",
    };
    expect(errorCause(refused)).toBe("der Server hat mit Fehler 400 geantwortet (Name is too long)");
    // Why the connection failed.
    const noConnection = {
      code: "network",
      reason: "error sending request: tcp connect error: connection refused",
      detail: "network error: error sending request",
    };
    expect(errorCause(noConnection)).toBe(
      "die Verbindung zum Server ist fehlgeschlagen (error sending request: tcp connect error: connection refused)",
    );
    // The I/O error after the prefix, without the prefix again.
    const denied = { code: "settingsExport", reason: "Access is denied.", detail: "export failed: Access is denied." };
    expect(errorMessage(denied)).toBe(
      "Etwas ist schiefgelaufen: die Einstellungen ließen sich nicht exportieren (Access is denied.)",
    );
    setLocale("en", { reload: false });
    expect(errorCause(denied)).toBe("the settings could not be exported (Access is denied.)");
    // Not where it is noise: cpal's text after an output that did not open.
    const cpal = { code: "defaultOutputFailed", reason: "The device is busy", detail: "…" };
    expect(errorCause(cpal)).toBe(m.error_default_output_failed());
    // No reason, no brackets.
    expect(errorCause({ code: "database", detail: "database error: x" })).toBe(m.error_database());
    // Nor for a code with no words: its detail says it all.
    expect(errorCause({ code: "other", reason: "r", detail: "d" })).toBe("d");
  });

  it("puts the value a code names into its sentence", () => {
    const redirect = {
      code: "serverRedirect",
      status: 308,
      arg: "https://music.example",
      detail: "server returned 308: the server redirects to https://music.example — sign in…",
    };
    expect(errorCause(redirect)).toBe(m.error_server_redirect({ address: "https://music.example" }));
    expect(errorCause({ code: "sampleRate", arg: "3", detail: "…" })).toBe(
      m.error_sample_rate({ rate: "3" }),
    );
  });

  it("shows the detail of a code it has no words for", () => {
    for (const code of ["other", "audio", "update:install", "somethingNew", "toString"]) {
      expect(errorCause({ code, detail: "invalid search category" })).toBe("invalid search category");
    }
  });

  it("takes what has no code as it arrived", () => {
    // A build before the codes, a test mock, an exception in the WebView.
    expect(errorCause("album has no tracks")).toBe("album has no tracks");
    expect(errorCause(new Error("boom"))).toBe("boom");
    expect(errorCause(undefined)).toBe("undefined");
    expect(errorCause({ message: "no code" })).toBe('{"message":"no code"}');
    expect(errorMessage("x")).toBe(m.error_generic({ message: "x" }));
  });
});

describe("errorReport", () => {
  it("keeps what the report carries and drops what has the wrong type", () => {
    expect(errorReport({ code: "server", status: 404, arg: 5, detail: "d" })).toEqual({
      code: "server",
      status: 404,
      arg: undefined,
      detail: "d",
    });
    expect(errorDetail(albumEmpty)).toBe("album has no tracks");
    // An area's own code still reads where its parser looks for it.
    expect(errorDetail({ code: "update:playing", detail: "update:playing" })).toBe("update:playing");
  });

  it("tells a deleted item from a failed request", () => {
    expect(isNotFound({ code: "server", status: 404, detail: "server returned 404: gone" })).toBe(true);
    expect(isNotFound({ code: "server", status: 500, detail: "server returned 500: returned 404" })).toBe(
      false,
    );
    expect(isNotFound({ code: "network", detail: "network error: timed out" })).toBe(false);
    // Only the status counts, not what a text says.
    expect(isNotFound("server returned 404: album a not found")).toBe(false);
  });
});

describe("the contract with error.rs", () => {
  it("has words for every code Rust names", () => {
    // Every entry of `KNOWN`: as many codes read as there are shapes.
    const table = /const KNOWN: &\[\(&str, Shape\)\] = &\[([\s\S]*?)\n\];/.exec(rust)?.[1] ?? "";
    const known = [...table.matchAll(/\(\s*"([^"]+)",\s*Shape::/g)].map((match) => match[1]);
    expect(known.length).toBeGreaterThan(40);
    expect(known.length).toBe(table.match(/Shape::/g)?.length);
    // The variants' own codes, as `report` names them; `other` and `audio`
    // are the ones with no words, whose detail is the message.
    const report = /pub fn report\(&self\)[\s\S]*?\n {4}\}/.exec(rust)?.[0] ?? "";
    const base = [...report.matchAll(/=> \{?\s*\(\s*"([^"]+)",/g)].map((match) => match[1]);
    expect(base.sort()).toEqual([
      "audio",
      "credentialStore",
      "database",
      "network",
      "notConnected",
      "other",
      "server",
    ]);
    const missing = [...new Set([...known, ...base])]
      .filter((code) => code !== "other" && code !== "audio")
      .filter((code) => !WORDED_CODES.includes(code));
    expect(missing).toEqual([]);
  });

  it("has every message in both languages", () => {
    const catalogs = import.meta.glob("/messages/*.json", { import: "default", eager: true }) as Record<
      string,
      Record<string, string>
    >;
    const keys = (lang: string) =>
      Object.keys(catalogs[`/messages/${lang}.json`])
        .filter((key) => key.startsWith("error_"))
        .sort();
    expect(keys("en").length).toBeGreaterThan(50);
    expect(keys("de")).toEqual(keys("en"));
  });

  it("words every code in both languages", () => {
    const locale = getLocale();
    const worded = { en: new Map<string, string>(), de: new Map<string, string>() };
    try {
      for (const lang of ["en", "de"] as const) {
        setLocale(lang, { reload: false });
        for (const code of WORDED_CODES) {
          const text = errorCause({ code, status: 500, arg: "x", detail: "DETAIL" });
          expect(text, `${lang} ${code}`).not.toBe("DETAIL");
          expect(text, `${lang} ${code}`).not.toBe("");
          worded[lang].set(code, text);
        }
      }
      // A message missing in German falls back to English, so it would read
      // the same in both.
      const same = WORDED_CODES.filter((code) => worded.de.get(code) === worded.en.get(code));
      expect(same).toEqual([]);
    } finally {
      setLocale(locale, { reload: false });
    }
  });
});
