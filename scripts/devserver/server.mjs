// A stand-in Jellyfin server for local development, demos and screenshots.
//
// It speaks just enough of the Jellyfin API for the app to sign in, browse,
// play and visualize: the client's wire DTOs are `#[serde(default)]` all the
// way down (src-tauri/src/api/types.rs), so a response only has to carry the
// fields it actually wants populated.
//
//   node scripts/devserver/server.mjs [--port 8096]
//
// Then sign the app in at http://127.0.0.1:8096 with any username/password.
// Media is generated on first run into a cache directory outside the repo.
//
// This is NOT a Jellyfin implementation. It ignores authentication entirely,
// keeps everything in memory and answers only the routes below -- enough to
// work with, not something to point at a real client you care about.
import { randomUUID } from "node:crypto";
import { createReadStream, statSync, readFileSync } from "node:fs";
import { createServer } from "node:http";
import { homedir } from "node:os";
import { join } from "node:path";
import {
  collections,
  genreId,
  library,
  musicGenreId,
  musicLibraryId,
  playlists,
  user,
  TICKS_PER_MS,
  idFor,
} from "./fixtures.mjs";
import { AUDIO_SECONDS, ensureMedia } from "./media.mjs";

const argv = process.argv.slice(2);
const portIdx = argv.indexOf("--port");
const PORT = portIdx >= 0 ? Number(argv[portIdx + 1]) : 8096;
/** Log every request. Useful when you want to see what a client actually asks
 *  for, or to watch playback reporting arrive while the window is hidden. */
const VERBOSE = argv.includes("--verbose");
/** Report the audio's real length instead of the invented one. The library
 *  then looks less real, but a track ends where the server says it does:
 *  prefetch, gapless hand-off and crossfades only happen with that. */
const REAL_LENGTHS = argv.includes("--real-lengths");
const lengthMs = (track) => (REAL_LENGTHS ? AUDIO_SECONDS * 1000 : track.durationMs);
const MEDIA = join(homedir(), ".cache", "sonarune", "devserver-media");

const favorites = new Set();
const playCounts = new Map();
/** When a track was last played (ISO-8601), as playback reports it. */
const lastPlayed = new Map();

// --- item shaping ------------------------------------------------------------

const albumById = new Map(library.albums.map((a) => [a.id, a]));
const trackById = new Map(library.tracks.map((t) => [t.id, t]));
const tracksByAlbum = new Map();
for (const t of library.tracks) {
  if (!tracksByAlbum.has(t.albumId)) tracksByAlbum.set(t.albumId, []);
  tracksByAlbum.get(t.albumId).push(t);
}

function userData(id) {
  return {
    IsFavorite: favorites.has(id),
    PlayCount: playCounts.get(id) ?? 0,
    Played: (playCounts.get(id) ?? 0) > 0,
    PlaybackPositionTicks: 0,
    LastPlayedDate: lastPlayed.get(id),
  };
}

function albumItem(album) {
  return {
    Id: album.id,
    Name: album.name,
    Type: "MusicAlbum",
    AlbumArtist: album.artistName,
    AlbumArtists: [{ Id: album.artistId, Name: album.artistName }],
    Artists: [album.artistName],
    ProductionYear: album.year,
    ChildCount: album.trackCount,
    Genres: [album.genre],
    GenreItems: [{ Id: musicGenreId(album.genre), Name: album.genre }],
    ImageTags: { Primary: album.id },
    ImageBlurHashes: {},
    DateCreated: `20${20 + (album.year % 5)}-03-14T10:00:00.0000000Z`,
    RunTimeTicks:
      (tracksByAlbum.get(album.id) ?? []).reduce((s, t) => s + lengthMs(t), 0) * TICKS_PER_MS,
    UserData: userData(album.id),
  };
}

function trackItem(track) {
  const album = albumById.get(track.albumId);
  return {
    Id: track.id,
    Name: track.name,
    Type: "Audio",
    Album: track.albumName,
    AlbumId: track.albumId,
    AlbumArtist: track.artistName,
    Artists: [track.artistName],
    ArtistItems: [{ Id: track.artistId, Name: track.artistName }],
    ProductionYear: track.year,
    IndexNumber: track.indexNumber,
    ParentIndexNumber: 1,
    RunTimeTicks: lengthMs(track) * TICKS_PER_MS,
    Genres: [track.genre],
    GenreItems: [{ Id: musicGenreId(track.genre), Name: track.genre }],
    ImageTags: {},
    AlbumPrimaryImageTag: album ? album.id : undefined,
    ImageBlurHashes: {},
    NormalizationGain: -2.5,
    DateCreated: "2024-03-14T10:00:00.0000000Z",
    UserData: userData(track.id),
  };
}

function artistItem(artist) {
  const albums = library.albums.filter((a) => a.artistId === artist.id);
  return {
    Id: artist.id,
    Name: artist.name,
    Type: "MusicArtist",
    ChildCount: albums.length,
    ImageTags: albums[0] ? { Primary: albums[0].id } : {},
    ImageBlurHashes: {},
    Overview: `${artist.name} is part of the Sonarune demo library. Nothing here is a real recording.`,
    UserData: userData(artist.id),
  };
}

function playlistItem(pl) {
  return {
    Id: pl.id,
    Name: pl.name,
    Type: "Playlist",
    MediaType: "Audio",
    // 10.11 counts a playlist's distinct items: a track in it twice is one.
    ChildCount: new Set(pl.trackIds).size,
    ImageTags: {},
    ImageBlurHashes: {},
    RunTimeTicks:
      pl.trackIds.reduce((s, id) => {
        const t = library.tracks.find((x) => x.id === id);
        return s + (t ? lengthMs(t) : 0);
      }, 0) * TICKS_PER_MS,
    UserData: userData(pl.id),
  };
}

function collectionItem(set) {
  return {
    Id: set.id,
    Name: set.name,
    Type: "BoxSet",
    ChildCount: set.albumIds.length + set.otherMembers,
    ImageTags: {},
    ImageBlurHashes: {},
    UserData: userData(set.id),
  };
}

/** A genre id of either kind (see fixtures.mjs) back to its name. Jellyfin's
 *  `genreIds` filter matches by name too, which is why both kinds work there. */
function genreName(id) {
  return library.genres.find((g) => g.musicGenreId === id || g.genreId === id)?.name ?? null;
}

/** A genre as an item: the `MusicGenre` tracks name, or its `Genre` twin. */
function genreItem(g, music) {
  return {
    Id: music ? g.musicGenreId : g.genreId,
    Name: g.name,
    Type: music ? "MusicGenre" : "Genre",
    ImageTags: {},
    ImageBlurHashes: {},
  };
}

// --- /Items --------------------------------------------------------------------

function pick(q, ...names) {
  for (const n of names) {
    const v = q.get(n);
    if (v !== null && v !== "") return v;
  }
  return null;
}

function csv(v) {
  return v ? v.split(",").map((s) => s.trim()).filter(Boolean) : [];
}

function queryItems(q) {
  const types = csv(pick(q, "IncludeItemTypes", "includeItemTypes"));
  const parentId = pick(q, "ParentId", "parentId");
  const search = (pick(q, "SearchTerm", "searchTerm") ?? "").toLowerCase();
  const filters = csv(pick(q, "Filters", "filters"));
  const artistIds = csv(pick(q, "AlbumArtistIds", "albumArtistIds", "ArtistIds", "artistIds"));
  const genreIds = csv(pick(q, "GenreIds", "genreIds"));
  const years = csv(pick(q, "Years", "years"));
  const ids = csv(pick(q, "Ids", "ids"));
  const nameLessThan = pick(q, "NameLessThan", "nameLessThan");
  const nameStartsWith = pick(q, "NameStartsWith", "nameStartsWith");
  const sortBy = csv(pick(q, "SortBy", "sortBy"))[0] ?? "SortName";
  // One order per sort key ("Descending,Ascending"); only the first key sorts here.
  const descending = csv(pick(q, "SortOrder", "sortOrder"))[0]?.toLowerCase() === "descending";
  const start = Number(pick(q, "StartIndex", "startIndex") ?? 0);
  const limitRaw = pick(q, "Limit", "limit");
  const limit = limitRaw === null ? null : Number(limitRaw);

  let rows;
  if (ids.length && types.length === 0) {
    // A lookup by id with no type given. Real Jellyfin searches the whole
    // library here, and the app leans on that to fetch a single artist or
    // playlist — falling through to the album default below made both detail
    // pages answer "not found" against this server while their lists worked.
    rows = [
      ...library.albums.map((a) => ({ kind: "album", src: a, item: albumItem(a) })),
      ...library.tracks.map((t) => ({ kind: "track", src: t, item: trackItem(t) })),
      ...library.artists.map((a) => ({ kind: "artist", src: a, item: artistItem(a) })),
      ...playlists.map((p) => ({ kind: "playlist", src: p, item: playlistItem(p) })),
      ...collections.map((c) => ({ kind: "collection", src: c, item: collectionItem(c) })),
      // Genres too, of both kinds: what a smart view saved before 0.6.1
      // holds is looked up so.
      ...library.genres.flatMap((g) =>
        [true, false].map((music) => ({
          kind: "genre",
          src: { id: music ? g.musicGenreId : g.genreId, name: g.name },
          item: genreItem(g, music),
        })),
      ),
    ];
  } else if (types.includes("Playlist")) {
    rows = playlists.map((p) => ({ kind: "playlist", src: p, item: playlistItem(p) }));
  } else if (types.includes("MusicArtist") || types.includes("MusicAlbumArtist")) {
    rows = library.artists.map((a) => ({ kind: "artist", src: a, item: artistItem(a) }));
  } else if (types.includes("Audio")) {
    rows = library.tracks.map((t) => ({ kind: "track", src: t, item: trackItem(t) }));
  } else if (types.includes("MusicAlbum") || types.length === 0) {
    rows = library.albums.map((a) => ({ kind: "album", src: a, item: albumItem(a) }));
  } else if (types.includes("BoxSet")) {
    rows = collections.map((c) => ({ kind: "collection", src: c, item: collectionItem(c) }));
  } else {
    rows = [];
  }

  if (parentId) {
    const set = collections.find((c) => c.id === parentId);
    if (set) {
      // A box set's members are linked, not contained.
      rows = rows.filter((r) => r.kind === "album" && set.albumIds.includes(r.src.id));
    } else if (parentId !== musicLibraryId) {
      rows = rows.filter(
        (r) => r.src.albumId === parentId || r.src.artistId === parentId || r.src.id === parentId,
      );
    }
  }
  if (ids.length) rows = rows.filter((r) => ids.includes(r.src.id));
  if (artistIds.length) rows = rows.filter((r) => artistIds.includes(r.src.artistId));
  if (genreIds.length) {
    const names = genreIds.map(genreName).filter(Boolean);
    rows = rows.filter((r) => r.src.genre && names.includes(r.src.genre));
  }
  if (years.length) rows = rows.filter((r) => years.includes(String(r.src.year)));
  if (search) rows = rows.filter((r) => r.src.name.toLowerCase().includes(search));
  if (nameLessThan) rows = rows.filter((r) => r.src.name.localeCompare(nameLessThan) < 0);
  if (nameStartsWith) {
    rows = rows.filter((r) => r.src.name.toLowerCase().startsWith(nameStartsWith.toLowerCase()));
  }
  if (filters.includes("IsFavorite")) rows = rows.filter((r) => favorites.has(r.src.id));
  if (filters.includes("IsPlayed")) rows = rows.filter((r) => (playCounts.get(r.src.id) ?? 0) > 0);

  const key = {
    SortName: (r) => r.src.name,
    Name: (r) => r.src.name,
    Album: (r) => r.src.albumName ?? r.src.name,
    AlbumArtist: (r) => r.src.artistName ?? "",
    ProductionYear: (r) => r.src.year ?? 0,
    DateCreated: (r) => r.src.year ?? 0,
    DatePlayed: (r) => lastPlayed.get(r.src.id) ?? "",
    PlayCount: (r) => playCounts.get(r.src.id) ?? 0,
    Random: () => Math.random(),
    ParentIndexNumber: (r) => r.src.indexNumber ?? 0,
    IndexNumber: (r) => r.src.indexNumber ?? 0,
  }[sortBy] ?? ((r) => r.src.name);

  rows.sort((a, b) => {
    const ka = key(a);
    const kb = key(b);
    const c = typeof ka === "number" ? ka - kb : String(ka).localeCompare(String(kb));
    return descending ? -c : c;
  });

  const total = rows.length;
  const page = rows.slice(start, limit === null ? undefined : start + limit);
  return { Items: page.map((r) => r.item), TotalRecordCount: total, StartIndex: start };
}

// --- http ----------------------------------------------------------------------

// Every answer allows any origin, as Jellyfin does by default: browser-engine
// clients pointed at this server (the bench drives one) need that. Nothing but
// the fake library and its generated media is ever served.

function json(res, body, status = 200) {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Content-Length": Buffer.byteLength(text),
    "Access-Control-Allow-Origin": "*",
  });
  res.end(text);
}

function noContent(res) {
  res.writeHead(204, { "Access-Control-Allow-Origin": "*" });
  res.end();
}

/**
 * The byte range a `Range` header asks for, within a file of `size` bytes:
 * `{ start, end }` (inclusive), `null` without a usable header (send it all),
 * or `"unsatisfiable"` for a range that lies outside the file.
 */
function byteRange(header, size) {
  const m = header && /^bytes=(\d*)-(\d*)$/.exec(header.trim());
  if (!m || (m[1] === "" && m[2] === "")) return null;
  if (m[1] === "") {
    // A suffix range: the last N bytes.
    const length = Number(m[2]);
    if (length === 0 || size === 0) return "unsatisfiable";
    return { start: Math.max(0, size - length), end: size - 1 };
  }
  const start = Number(m[1]);
  const end = m[2] === "" ? size - 1 : Math.min(Number(m[2]), size - 1);
  if (start >= size || start > end) return "unsatisfiable";
  return { start, end };
}

function sendFile(res, path, type, rangeHeader) {
  let stat;
  try {
    stat = statSync(path);
  } catch {
    res.writeHead(404).end();
    return;
  }
  // Seeking a direct-play file goes through Range; without it the player can
  // only ever start from zero.
  const range = byteRange(rangeHeader, stat.size);
  if (range === "unsatisfiable") {
    res.writeHead(416, {
      "Content-Range": `bytes */${stat.size}`,
      "Access-Control-Allow-Origin": "*",
    });
    res.end();
    return;
  }
  const { start, end } = range ?? { start: 0, end: stat.size - 1 };
  res.writeHead(range ? 206 : 200, {
    "Content-Type": type,
    "Content-Length": stat.size === 0 ? 0 : end - start + 1,
    ...(range ? { "Content-Range": `bytes ${start}-${end}/${stat.size}` } : {}),
    "Accept-Ranges": "bytes",
    "Access-Control-Allow-Origin": "*",
  });
  if (stat.size === 0) {
    res.end();
    return;
  }
  createReadStream(path, { start, end })
    // A file that vanished after the stat ends the response, not the server.
    .on("error", () => res.destroy())
    .pipe(res);
}

function readBody(req) {
  return new Promise((resolve) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
  });
}

/** The request's JSON body, or `{}` for a missing or malformed one. */
async function readJson(req) {
  try {
    const parsed = JSON.parse((await readBody(req)) || "{}");
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

/** `ids` without repeats, first one first — 10.11 adds a playlist's tracks so. */
function distinct(ids) {
  return [...new Set(ids)];
}

/**
 * Move the first entry of `itemId` to `newIndex`, the way 10.11 does it: the
 * entry is found by its item id (the first copy of a repeated track), and the
 * slot by the track that was at `newIndex - 1` — its first copy, too.
 */
function moveEntry(trackIds, itemId, index) {
  const from = trackIds.indexOf(itemId);
  const newIndex = Math.min(Math.max(index, 0), trackIds.length - 1);
  if (from < 0 || from === newIndex) return trackIds;
  const prior = trackIds[Math.max(newIndex - 1, 0)];
  const priorIndex = trackIds.indexOf(prior);
  const to = newIndex === 0 ? (priorIndex > 0 ? priorIndex - 1 : 0) : priorIndex + 1;
  const next = trackIds.filter((_, i) => i !== from);
  next.splice(Math.min(to, next.length), 0, itemId);
  return next;
}

/** The demo user's one library — what `/UserViews` lists. */
function musicView() {
  return {
    Id: musicLibraryId,
    Name: "Music",
    Type: "CollectionFolder",
    CollectionType: "music",
    ImageTags: {},
    ImageBlurHashes: {},
  };
}

/** Track ids from a request: known tracks only. */
function trackIdsOf(values) {
  return (Array.isArray(values) ? values : []).filter(
    (id) => typeof id === "string" && trackById.has(id),
  );
}

// A request that throws (a bad header, a bug in here) answers 500; it must not
// take the server down with it, and every client connected to it.
const server = createServer((req, res) => {
  handle(req, res).catch((error) => {
    console.error(`  (failed) ${req.method} ${req.url}: ${error?.stack ?? error}`);
    if (res.headersSent) res.destroy();
    else json(res, { error: "internal error" }, 500);
  });
});

async function handle(req, res) {
  // Only the path and the query matter, so a fixed base: a malformed Host
  // header must not be able to make the URL unparseable.
  const url = new URL(req.url ?? "/", "http://127.0.0.1");
  if (VERBOSE) {
    console.log(`${new Date().toISOString().slice(11, 19)} ${req.method} ${url.pathname}`);
  }
  // Jellyfin runs on ASP.NET, whose routing ignores case -- and clients rely on
  // that: Feishin asks for `/users/authenticatebyname`. Match on a lowercased
  // copy and keep the original only where an id is read out of it.
  const p = url.pathname.toLowerCase();
  const q = url.searchParams;

  if (req.method === "OPTIONS") {
    res.writeHead(204, {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Headers": "*",
      "Access-Control-Allow-Methods": "GET,POST,DELETE,OPTIONS",
    });
    res.end();
    return;
  }

  // --- handshake ---
  if (p === "/system/info/public" || p === "/system/info") {
    return json(res, { Version: "10.11.0", ServerName: "Sonarune dev", Id: idFor("server", "dev") });
  }
  if (p === "/users/authenticatebyname") {
    await readBody(req);
    return json(res, { User: user, AccessToken: "dev-token", SessionInfo: {} });
  }
  if (p === "/users/me") return json(res, user);

  // The older, user-scoped shape of the browse API. Sonarune does not use it;
  // other clients do, and a stand-in server that only answers one client is
  // not much of a stand-in.
  let um = /^\/users\/([0-9a-f-]+)(\/(views|items|items\/([0-9a-f]+)))?$/.exec(p);
  if (um) {
    const sub = um[3];
    if (!sub) return json(res, user);
    if (sub === "views") return json(res, { Items: [musicView()], TotalRecordCount: 1 });
    if (um[4]) {
      const one = albumById.get(um[4])
        ? albumItem(albumById.get(um[4]))
        : library.tracks.find((t) => t.id === um[4])
          ? trackItem(library.tracks.find((t) => t.id === um[4]))
          : library.artists.find((a) => a.id === um[4])
            ? artistItem(library.artists.find((a) => a.id === um[4]))
            : null;
      return one ? json(res, one) : json(res, { error: "not found" }, 404);
    }
    return json(res, queryItems(q));
  }
  if (p === "/sessions/logout") return noContent(res);
  if (p.startsWith("/quickconnect/")) return json(res, { Authenticated: false, Secret: "", Code: "000000" });

  // --- playback reporting ---
  if (p.startsWith("/sessions/playing")) {
    const body = await readBody(req);
    if (p.endsWith("/stopped")) {
      try {
        const id = JSON.parse(body || "{}").ItemId;
        if (id) {
          playCounts.set(id, (playCounts.get(id) ?? 0) + 1);
          lastPlayed.set(id, new Date().toISOString());
        }
      } catch {
        /* the demo server does not care about a malformed report */
      }
    }
    return noContent(res);
  }

  // --- favorites ---
  let m = /^\/userfavoriteitems\/([0-9a-f]+)$/.exec(p);
  if (m) {
    if (req.method === "DELETE") favorites.delete(m[1]);
    else favorites.add(m[1]);
    return json(res, userData(m[1]));
  }

  // --- images ---
  m = /^\/items\/([0-9a-f]+)\/images\/primary$/.exec(p);
  if (m) {
    // Covers exist for albums only; anything else names its album in `tag`.
    // Never a file name taken from the request: `tag=../../..` walked out of
    // the media directory to any PNG on the disk.
    const album = albumById.get(m[1]) ?? albumById.get(q.get("tag") ?? "");
    if (!album) return json(res, { error: "not found" }, 404);
    return sendFile(res, join(MEDIA, "covers", `${album.id}.png`), "image/png", req.headers.range);
  }

  // --- audio ---
  m = /^\/audio\/([0-9a-f]+)\/(universal|stream[^/]*)$/.exec(p);
  if (m) return sendFile(res, join(MEDIA, "audio", `${m[1]}.flac`), "audio/flac", req.headers.range);

  m = /^\/audio\/([0-9a-f]+)\/lyrics$/.exec(p);
  if (m) return json(res, {}, 404);

  // --- browsing ---
  if (p === "/userviews") return json(res, { Items: [musicView()], TotalRecordCount: 1 });
  if (p === "/items") return json(res, queryItems(q));
  if (p === "/artists" || p === "/artists/albumartists") {
    const rows = library.artists.map(artistItem);
    return json(res, { Items: rows, TotalRecordCount: rows.length });
  }
  if (p === "/genres" || p === "/musicgenres") {
    // As 10.11 answers: music genres below a music library (and from the
    // obsolete /MusicGenres); without one, the `Genre` items, which exist only
    // for genres something besides music carries too — here, a playlist.
    const music = p === "/musicgenres" || pick(q, "ParentId", "parentId") === musicLibraryId;
    const onPlaylists = new Set(
      playlists.flatMap((pl) => pl.trackIds.map((id) => trackById.get(id)?.genre)),
    );
    const rows = library.genres
      .filter((g) => music || onPlaylists.has(g.name))
      .map((g) => genreItem(g, music));
    return json(res, { Items: rows, TotalRecordCount: rows.length });
  }
  if (p === "/years") {
    const years = [...new Set(library.albums.map((a) => a.year))].sort();
    return json(res, { Items: years.map((y) => ({ Id: String(y), Name: String(y) })), TotalRecordCount: years.length });
  }

  m = /^\/items\/([0-9a-f]+)\/instantmix$/.exec(p);
  if (m) {
    const shuffled = [...library.tracks].sort(() => Math.random() - 0.5).slice(0, 25);
    return json(res, { Items: shuffled.map(trackItem), TotalRecordCount: shuffled.length });
  }

  m = /^\/artists\/([0-9a-f]+)\/similar$/.exec(p);
  if (m) {
    const others = library.artists.filter((a) => a.id !== m[1]).slice(0, 6);
    return json(res, { Items: others.map(artistItem), TotalRecordCount: others.length });
  }

  // --- playlists, kept the way Jellyfin 10.11 keeps them: an entry is known
  // by its item id only, a track goes in once, and removing or moving by an
  // entry id hits every copy or the first one ---
  if (p === "/playlists" && req.method === "POST") {
    const body = await readJson(req);
    const pl = {
      id: randomUUID().replaceAll("-", ""),
      name: typeof body.Name === "string" && body.Name.trim() ? body.Name.trim() : "Playlist",
      trackIds: distinct(trackIdsOf(body.Ids)),
    };
    playlists.push(pl);
    return json(res, { Id: pl.id });
  }

  m = /^\/playlists\/([0-9a-f]+)$/.exec(p);
  if (m && req.method === "POST") {
    const pl = playlists.find((x) => x.id === m[1]);
    if (!pl) return json(res, { error: "not found" }, 404);
    const body = await readJson(req);
    if (typeof body.Name === "string" && body.Name.trim()) pl.name = body.Name.trim();
    // `Ids` replaces the entries, each item once.
    if (Array.isArray(body.Ids)) pl.trackIds = distinct(trackIdsOf(body.Ids));
    return noContent(res);
  }
  if (m && req.method === "GET") {
    // Who may open it, and the item id of every entry in order — repeats, and
    // in Jellyfin the entries hidden from the user, included (nothing is
    // hidden here).
    const pl = playlists.find((x) => x.id === m[1]);
    if (!pl) return json(res, { error: "not found" }, 404);
    return json(res, { OpenAccess: false, Shares: [], ItemIds: pl.trackIds });
  }

  m = /^\/playlists\/([0-9a-f]+)\/items\/([0-9a-f]+)\/move\/(\d+)$/.exec(p);
  if (m && req.method === "POST") {
    const pl = playlists.find((x) => x.id === m[1]);
    if (!pl) return json(res, { error: "not found" }, 404);
    pl.trackIds = moveEntry(pl.trackIds, m[2], Number(m[3]));
    return noContent(res);
  }

  m = /^\/playlists\/([0-9a-f]+)\/items$/.exec(p);
  if (m) {
    const pl = playlists.find((x) => x.id === m[1]);
    if (!pl) return json(res, { error: "not found" }, 404);
    if (req.method === "POST") {
      // What the playlist holds already, and repeats within the request, are
      // skipped — and the answer is a 204 all the same.
      const have = new Set(pl.trackIds);
      const adding = distinct(trackIdsOf(csv(pick(q, "Ids", "ids")))).filter((id) => !have.has(id));
      pl.trackIds = [...pl.trackIds, ...adding];
      return noContent(res);
    }
    if (req.method === "DELETE") {
      const remove = new Set(csv(pick(q, "EntryIds", "entryIds")));
      pl.trackIds = pl.trackIds.filter((id) => !remove.has(id));
      return noContent(res);
    }
    const items = pl.trackIds
      .map((id) => trackById.get(id))
      .filter(Boolean)
      .map((t) => ({ ...trackItem(t), PlaylistItemId: t.id }));
    return json(res, { Items: items, TotalRecordCount: items.length });
  }

  m = /^\/items\/([0-9a-f]+)$/.exec(p);
  if (m && req.method === "DELETE") {
    const index = playlists.findIndex((x) => x.id === m[1]);
    if (index < 0) return json(res, { error: "not found" }, 404);
    playlists.splice(index, 1);
    return noContent(res);
  }
  if (m) {
    const id = m[1];
    const album = albumById.get(id);
    if (album) return json(res, albumItem(album));
    const track = library.tracks.find((t) => t.id === id);
    if (track) return json(res, trackItem(track));
    const artist = library.artists.find((a) => a.id === id);
    if (artist) return json(res, artistItem(artist));
    const pl = playlists.find((x) => x.id === id);
    if (pl) return json(res, playlistItem(pl));
    return json(res, { error: "not found" }, 404);
  }

  // Anything else: an empty, well-formed page beats a 500 in a demo.
  console.log(`  (unhandled) ${req.method} ${url.pathname}`);
  return json(res, { Items: [], TotalRecordCount: 0 });
}

const made = ensureMedia(MEDIA, library.albums, library.tracks);
if (made.madeCovers || made.madeAudio) {
  console.log(`generated ${made.madeCovers} covers, ${made.madeAudio} audio files in ${MEDIA}`);
}

server.listen(PORT, "127.0.0.1", () => {
  console.log(`Sonarune dev server on http://127.0.0.1:${PORT}`);
  console.log(
    `  ${library.artists.length} artists, ${library.albums.length} albums, ` +
      `${library.tracks.length} tracks, ${playlists.length} playlists`,
  );
  console.log("  sign in with any username and password");
});
