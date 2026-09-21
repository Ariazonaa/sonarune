// The fake library. Invented artists, albums and tracks -- nothing here comes
// from anyone's real collection, which is the point: screenshots and demos show
// the app, not the person running it.
//
// Ids are stable 32-char hex strings derived from the name, so a cover cached
// under one id stays valid across restarts.
import { createHash } from "node:crypto";

/** Jellyfin item ids are GUIDs without dashes; derive one from a string. */
export function idFor(kind, name) {
  return createHash("sha1").update(`${kind}:${name}`).digest("hex").slice(0, 32);
}

/** Jellyfin measures durations in 100-nanosecond ticks. */
export const TICKS_PER_MS = 10_000;

const GENRES = ["Ambient", "Electronic", "Indie", "Post-Rock", "Jazz", "Folk"];

/** [artist, [album, year, genre, trackTitles]] */
const LIBRARY = [
  ["Halcyon Field", [
    ["Paper Lanterns", 2021, "Ambient",
      ["First Light", "Paper Lanterns", "Slow Tide", "Hollow Season", "The Long Way Down", "Afterglow"]],
    ["Long Exposure", 2023, "Post-Rock",
      ["Aperture", "Long Exposure", "Grain", "Static Bloom", "Dust and Signal"]],
  ]],
  ["Vela Nine", [
    ["Orbital Drift", 2022, "Electronic",
      ["Ascent", "Orbital Drift", "Low Earth", "Telemetry", "Night Side", "Re-entry", "Splashdown"]],
  ]],
  ["The Quiet Machines", [
    ["Analog Summer", 2019, "Indie",
      ["Cassette Sunrise", "Analog Summer", "Pier Lights", "Sixteen Hours", "Warm Front"]],
    ["Second Light", 2024, "Indie",
      ["Second Light", "Marginalia", "Blue Wire", "Hold Still", "Everything Louder", "The Quiet Part"]],
  ]],
  ["Mira Solace", [
    ["Northbound", 2020, "Folk",
      ["Northbound", "Two Rivers", "Pine and Ash", "Winter Count", "Homing"]],
  ]],
  ["Cassette Weather", [
    ["Tape Hiss Lullabies", 2018, "Ambient",
      ["Hiss", "Lullaby for a Dial Tone", "Spooling", "Magnetic North", "Rewind", "End of Side A"]],
  ]],
  ["Ivory Static", [
    ["Blue Hour", 2023, "Electronic",
      ["Blue Hour", "Glass Corridor", "Half-Life", "Neon Rain", "Undertow"]],
  ]],
  ["Rowan & the Tide", [
    ["Saltwater Hymns", 2022, "Folk",
      ["Saltwater Hymn", "Low Water", "The Ferryman's Daughter", "Harbour Song", "Leaving Kirkwall"]],
  ]],
  ["Neon Cartography", [
    ["Maps of Nowhere", 2021, "Jazz",
      ["Meridian", "Maps of Nowhere", "Cartographer's Blues", "Due East", "Terra Incognita", "Last Known Position"]],
  ]],
];

function buildLibrary() {
  const artists = [];
  const albums = [];
  const tracks = [];

  for (const [artistName, albumSpecs] of LIBRARY) {
    const artistId = idFor("artist", artistName);
    artists.push({ id: artistId, name: artistName });

    for (const [albumName, year, genre, titles] of albumSpecs) {
      const albumId = idFor("album", `${artistName}/${albumName}`);
      albums.push({
        id: albumId,
        name: albumName,
        artistId,
        artistName,
        year,
        genre,
        trackCount: titles.length,
      });

      titles.forEach((title, i) => {
        // 2:40 to 5:20, deterministic per track so totals stay stable.
        const seconds = 160 + ((i * 37 + albumName.length * 13) % 160);
        tracks.push({
          id: idFor("track", `${albumId}/${title}`),
          name: title,
          albumId,
          albumName,
          artistId,
          artistName,
          year,
          genre,
          indexNumber: i + 1,
          durationMs: seconds * 1000,
        });
      });
    }
  }

  const genres = GENRES.map((name) => ({ id: idFor("genre", name), name }));
  return { artists, albums, tracks, genres };
}

export const library = buildLibrary();

/** A small set of playlists over the fake tracks. */
export const playlists = [
  {
    id: idFor("playlist", "Late Shift"),
    name: "Late Shift",
    trackIds: library.tracks.filter((_, i) => i % 7 === 0).slice(0, 14).map((t) => t.id),
  },
  {
    id: idFor("playlist", "Sunday Morning"),
    name: "Sunday Morning",
    trackIds: library.tracks.filter((_, i) => i % 5 === 2).slice(0, 11).map((t) => t.id),
  },
];

export const user = {
  Id: idFor("user", "demo"),
  Name: "demo",
  Policy: {
    IsAdministrator: true,
    EnableContentDeletion: true,
    EnableContentDeletionFromFolders: [],
  },
};
