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
import { createReadStream, statSync, readFileSync } from "node:fs";
import { createServer } from "node:http";
import { homedir } from "node:os";
import { join } from "node:path";
import { library, playlists, user, TICKS_PER_MS, idFor } from "./fixtures.mjs";
import { ensureMedia } from "./media.mjs";

const argv = process.argv.slice(2);
const portIdx = argv.indexOf("--port");
const PORT = portIdx >= 0 ? Number(argv[portIdx + 1]) : 8096;
/** Log every request. Useful when you want to see what a client actually asks
 *  for, or to watch playback reporting arrive while the window is hidden. */
const VERBOSE = argv.includes("--verbose");
const MEDIA = join(homedir(), ".cache", "jellysic", "devserver-media");

const favorites = new Set();
const playCounts = new Map();

// --- item shaping ------------------------------------------------------------

const albumById = new Map(library.albums.map((a) => [a.id, a]));
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
    GenreItems: [{ Id: idFor("genre", album.genre), Name: album.genre }],
    ImageTags: { Primary: album.id },
    ImageBlurHashes: {},
    DateCreated: `20${20 + (album.year % 5)}-03-14T10:00:00.0000000Z`,
    RunTimeTicks:
      (tracksByAlbum.get(album.id) ?? []).reduce((s, t) => s + t.durationMs, 0) * TICKS_PER_MS,
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
    RunTimeTicks: track.durationMs * TICKS_PER_MS,
    Genres: [track.genre],
    GenreItems: [{ Id: idFor("genre", track.genre), Name: track.genre }],
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
    Overview: `${artist.name} is part of the Jellysic demo library. Nothing here is a real recording.`,
    UserData: userData(artist.id),
  };
}

function playlistItem(pl) {
  return {
    Id: pl.id,
    Name: pl.name,
    Type: "Playlist",
    ChildCount: pl.trackIds.length,
    ImageTags: {},
    ImageBlurHashes: {},
    RunTimeTicks:
      pl.trackIds.reduce((s, id) => {
        const t = library.tracks.find((x) => x.id === id);
        return s + (t ? t.durationMs : 0);
      }, 0) * TICKS_PER_MS,
    UserData: userData(pl.id),
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
  const descending = (pick(q, "SortOrder", "sortOrder") ?? "").toLowerCase() === "descending";
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
    ];
  } else if (types.includes("Playlist")) {
    rows = playlists.map((p) => ({ kind: "playlist", src: p, item: playlistItem(p) }));
  } else if (types.includes("MusicArtist") || types.includes("MusicAlbumArtist")) {
    rows = library.artists.map((a) => ({ kind: "artist", src: a, item: artistItem(a) }));
  } else if (types.includes("Audio")) {
    rows = library.tracks.map((t) => ({ kind: "track", src: t, item: trackItem(t) }));
  } else if (types.includes("MusicAlbum") || types.length === 0) {
    rows = library.albums.map((a) => ({ kind: "album", src: a, item: albumItem(a) }));
  } else {
    rows = []; // BoxSet and friends: the demo library has none
  }

  if (parentId) {
    rows = rows.filter(
      (r) => r.src.albumId === parentId || r.src.artistId === parentId || r.src.id === parentId,
    );
  }
  if (ids.length) rows = rows.filter((r) => ids.includes(r.src.id));
  if (artistIds.length) rows = rows.filter((r) => artistIds.includes(r.src.artistId));
  if (genreIds.length) {
    rows = rows.filter((r) => r.src.genre && genreIds.includes(idFor("genre", r.src.genre)));
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
    DatePlayed: (r) => playCounts.get(r.src.id) ?? 0,
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

function sendFile(res, path, type, range) {
  let stat;
  try {
    stat = statSync(path);
  } catch {
    res.writeHead(404).end();
    return;
  }
  // Seeking a direct-play file goes through Range; without it the player can
  // only ever start from zero.
  const m = range && /^bytes=(\d*)-(\d*)$/.exec(range);
  if (m) {
    const start = m[1] ? Number(m[1]) : 0;
    const end = m[2] ? Number(m[2]) : stat.size - 1;
    res.writeHead(206, {
      "Content-Type": type,
      "Content-Length": end - start + 1,
      "Content-Range": `bytes ${start}-${end}/${stat.size}`,
      "Accept-Ranges": "bytes",
      "Access-Control-Allow-Origin": "*",
    });
    createReadStream(path, { start, end }).pipe(res);
    return;
  }
  res.writeHead(200, {
    "Content-Type": type,
    "Content-Length": stat.size,
    "Accept-Ranges": "bytes",
    "Access-Control-Allow-Origin": "*",
  });
  createReadStream(path).pipe(res);
}

function readBody(req) {
  return new Promise((resolve) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
  });
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host ?? "127.0.0.1"}`);
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
    return json(res, { Version: "10.11.0", ServerName: "Jellysic dev", Id: idFor("server", "dev") });
  }
  if (p === "/users/authenticatebyname") {
    await readBody(req);
    return json(res, { User: user, AccessToken: "dev-token", SessionInfo: {} });
  }
  if (p === "/users/me") return json(res, user);

  // The older, user-scoped shape of the browse API. Jellysic does not use it;
  // other clients do, and a stand-in server that only answers one client is
  // not much of a stand-in.
  let um = /^\/users\/([0-9a-f-]+)(\/(views|items|items\/([0-9a-f]+)))?$/.exec(p);
  if (um) {
    const sub = um[3];
    if (!sub) return json(res, user);
    if (sub === "views") {
      return json(res, {
        Items: [
          {
            Id: idFor("view", "music"),
            Name: "Music",
            Type: "CollectionFolder",
            CollectionType: "music",
            ImageTags: {},
            ImageBlurHashes: {},
          },
        ],
        TotalRecordCount: 1,
      });
    }
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
        if (id) playCounts.set(id, (playCounts.get(id) ?? 0) + 1);
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
    const album = albumById.get(m[1]);
    const id = album ? album.id : q.get("tag");
    return sendFile(res, join(MEDIA, "covers", `${id}.png`), "image/png", req.headers.range);
  }

  // --- audio ---
  m = /^\/audio\/([0-9a-f]+)\/(universal|stream[^/]*)$/.exec(p);
  if (m) return sendFile(res, join(MEDIA, "audio", `${m[1]}.flac`), "audio/flac", req.headers.range);

  m = /^\/audio\/([0-9a-f]+)\/lyrics$/.exec(p);
  if (m) return json(res, {}, 404);

  // --- browsing ---
  if (p === "/items") return json(res, queryItems(q));
  if (p === "/artists" || p === "/artists/albumartists") {
    const rows = library.artists.map(artistItem);
    return json(res, { Items: rows, TotalRecordCount: rows.length });
  }
  if (p === "/genres" || p === "/musicgenres") {
    const rows = library.genres.map((g) => ({
      Id: g.id,
      Name: g.name,
      Type: "MusicGenre",
      ImageTags: {},
      ImageBlurHashes: {},
    }));
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

  m = /^\/playlists\/([0-9a-f]+)\/items$/.exec(p);
  if (m) {
    const pl = playlists.find((x) => x.id === m[1]);
    const items = (pl?.trackIds ?? []).map((id, i) => {
      const t = library.tracks.find((x) => x.id === id);
      return { ...trackItem(t), PlaylistItemId: `${pl.id}-${i}` };
    });
    return json(res, { Items: items, TotalRecordCount: items.length });
  }

  m = /^\/items\/([0-9a-f]+)$/.exec(p);
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
});

const made = ensureMedia(MEDIA, library.albums, library.tracks);
if (made.madeCovers || made.madeAudio) {
  console.log(`generated ${made.madeCovers} covers, ${made.madeAudio} audio files in ${MEDIA}`);
}

server.listen(PORT, "127.0.0.1", () => {
  console.log(`Jellysic dev server on http://127.0.0.1:${PORT}`);
  console.log(
    `  ${library.artists.length} artists, ${library.albums.length} albums, ` +
      `${library.tracks.length} tracks, ${playlists.length} playlists`,
  );
  console.log("  sign in with any username and password");
});
