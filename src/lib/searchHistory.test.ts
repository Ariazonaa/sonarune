import { beforeEach, describe, expect, it } from "vitest";
import {
  clearSearchHistory,
  dropSharedSearchHistory,
  loadSearchHistory,
  saveSearchHistory,
  SEARCH_HISTORY_MAX,
} from "#lib/searchHistory.js";
import type { SessionInfo } from "#lib/types.js";

const account = (serverUrl: string, userId: string): SessionInfo => ({
  serverUrl,
  username: userId,
  userId,
  canDelete: false,
  canEdit: false,
});

const alice = account("https://music.home", "alice");
const bob = account("https://music.home", "bob");
const aliceElsewhere = account("https://other.home", "alice");

beforeEach(() => localStorage.clear());

describe("search history", () => {
  it("keeps each account's searches to itself", () => {
    saveSearchHistory(alice, ["private", "taste"]);
    expect(loadSearchHistory(alice)).toEqual(["private", "taste"]);
    expect(loadSearchHistory(bob)).toEqual([]);
    expect(loadSearchHistory(aliceElsewhere)).toEqual([]);
    expect(loadSearchHistory(null)).toEqual([]);
  });

  it("clears only the account it is asked to", () => {
    saveSearchHistory(alice, ["a"]);
    saveSearchHistory(bob, ["b"]);
    clearSearchHistory(alice);
    expect(loadSearchHistory(alice)).toEqual([]);
    expect(loadSearchHistory(bob)).toEqual(["b"]);
  });

  it("hands the old shared list to one account, and only one", () => {
    localStorage.setItem("sonarune:searchHistory", JSON.stringify(["before the update"]));
    expect(loadSearchHistory(alice)).toEqual(["before the update"]);
    expect(loadSearchHistory(alice)).toEqual(["before the update"]);
    expect(loadSearchHistory(bob)).toEqual([]);
    expect(localStorage.getItem("sonarune:searchHistory")).toBeNull();
  });

  it("does not let the shared list overwrite an account's own", () => {
    saveSearchHistory(alice, ["mine"]);
    localStorage.setItem("sonarune:searchHistory", JSON.stringify(["older"]));
    expect(loadSearchHistory(alice)).toEqual(["mine"]);
    expect(localStorage.getItem("sonarune:searchHistory")).toBeNull();
  });

  it("drops a shared list nobody adopted at sign-out", () => {
    localStorage.setItem("sonarune:searchHistory", JSON.stringify(["someone's"]));
    dropSharedSearchHistory();
    expect(loadSearchHistory(bob)).toEqual([]);
  });

  it("ignores corrupt and oversized entries", () => {
    localStorage.setItem("sonarune:searchHistory", "{not json");
    expect(loadSearchHistory(alice)).toEqual([]);
    const many = Array.from({ length: 20 }, (_, i) => `q${i}`);
    saveSearchHistory(bob, [...many, "", 3 as unknown as string]);
    expect(loadSearchHistory(bob)).toEqual(many.slice(0, SEARCH_HISTORY_MAX));
  });
});
