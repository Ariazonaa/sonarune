//! Play history, kept by the client itself.
//!
//! Jellyfin remembers per track how often it was played and when it was last
//! played, never when each play happened. A timeline and a review of a year
//! need the plays themselves, so the player writes one row per listen here:
//! a track counts by the ListenBrainz rule (`scrobble::should_scrobble`),
//! whether or not ListenBrainz is set up. The rows never leave the machine.
//!
//! Every row belongs to the account it was played on: signing in elsewhere
//! shows that account's history, not a mix of both. The account is its
//! Jellyfin user id, a GUID the server minted, so it names server and account
//! at once — and, unlike the address typed at sign-in, stays the same when
//! the server moves behind a proxy, to https or to another IP. Each row still
//! records the address it was played under.

use crate::api::types::{ArtistRef, TrackDto};
use crate::commands_library::{top_artists, ArtistPlays};
use crate::error::AppResult;
use crate::player::QueueTrack;
use crate::store::{keys, Store};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// What a history row remembers of a track: enough to show it and to play it
/// again, without the stream URL (which carries the server and goes stale).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryTrack {
    pub item_id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    #[serde(default)]
    pub album_id: Option<String>,
    /// The album's artist as the track was tagged; `None` in rows from
    /// before it was kept.
    #[serde(default)]
    pub album_artist: Option<String>,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub image_item_id: Option<String>,
    #[serde(default)]
    pub image_tag: Option<String>,
    #[serde(default)]
    pub image_blur_hash: Option<String>,
}

impl From<&QueueTrack> for HistoryTrack {
    fn from(track: &QueueTrack) -> Self {
        Self {
            item_id: track.item_id.clone(),
            name: track.name.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            album_id: track.album_id.clone(),
            album_artist: track.album_artist.clone(),
            artists: track.artists.clone(),
            duration_ms: track.duration_ms,
            image_item_id: track.image_item_id.clone(),
            image_tag: track.image_tag.clone(),
            image_blur_hash: track.image_blur_hash.clone(),
        }
    }
}

/// One listen.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// Grows with every row: the cursor for the next page.
    pub id: i64,
    /// When the listen ended, Unix milliseconds.
    pub played_at: i64,
    pub listened_ms: u64,
    pub track: HistoryTrack,
}

/// The years that have plays in them, newest first, and since when the client
/// has been recording at all — the review has to say that a year it only saw
/// part of is only part of a year.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryYears {
    pub years: Vec<i32>,
    pub recorded_since: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackPlays {
    pub track: HistoryTrack,
    pub plays: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumPlays {
    pub album_id: Option<String>,
    pub name: String,
    /// Whom the album is credited to: the album artist its tracks are
    /// tagged with, or else the artist most of its played tracks name.
    /// Empty when nobody is credited, or for a compilation
    /// ([`Self::various_artists`]).
    pub artist: String,
    /// No artist is named by most of its played tracks: a compilation.
    pub various_artists: bool,
    pub image_item_id: Option<String>,
    pub image_tag: Option<String>,
    pub image_blur_hash: Option<String>,
    pub plays: i64,
}

/// A year in plays.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YearReview {
    pub year: i32,
    pub plays: i64,
    pub listened_ms: u64,
    /// Different tracks played.
    pub tracks: i64,
    /// Different artists played, counted the way the artist chart counts.
    pub artists: i64,
    pub top_tracks: Vec<TrackPlays>,
    pub top_artists: Vec<ArtistPlays>,
    pub top_albums: Vec<AlbumPlays>,
    /// Plays per month, January first, in local time.
    pub months: [i64; 12],
    /// First and last play of the year, Unix milliseconds.
    pub first_played_at: Option<i64>,
    pub last_played_at: Option<i64>,
}

const TOP_TRACKS: usize = 10;
const TOP_ARTISTS: usize = 10;
const TOP_ALBUMS: usize = 10;
/// Upper bound for one page of the timeline.
pub const MAX_PAGE: u32 = 500;

/// The table. Created with the store (`Store::open`). The indexes serve the
/// timeline (an account's rows by id) and the years (by time); the one by
/// server address went with the address as part of the owner.
pub const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS play_history (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        server_url  TEXT    NOT NULL,
        user_id     TEXT    NOT NULL,
        played_at   INTEGER NOT NULL,
        listened_ms INTEGER NOT NULL,
        item_id     TEXT    NOT NULL,
        track       TEXT    NOT NULL
    );
    DROP INDEX IF EXISTS play_history_owner;
    CREATE INDEX IF NOT EXISTS play_history_user
        ON play_history (user_id, id);
    CREATE INDEX IF NOT EXISTS play_history_user_time
        ON play_history (user_id, played_at);";

/// The account the history is kept under right now; `None` while signed
/// out, when there is nothing to show.
fn account(store: &Store) -> Option<String> {
    store.get(keys::USER_ID)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Write a listen that ended at `played_at` (Unix milliseconds; now when
/// `None`). Returns whether a row was written.
pub fn record(
    store: &Store,
    track: &QueueTrack,
    listened_ms: u64,
    played_at: Option<i64>,
) -> AppResult<bool> {
    let played_at = played_at.unwrap_or_else(now_ms);
    record_at(store, &HistoryTrack::from(track), listened_ms, played_at)
}

fn record_at(
    store: &Store,
    track: &HistoryTrack,
    listened_ms: u64,
    played_at: i64,
) -> AppResult<bool> {
    let (Some(server), Some(user)) = (store.get(keys::SERVER_URL), account(store)) else {
        return Ok(false);
    };
    let json = serde_json::to_string(track)
        .map_err(|e| crate::error::AppError::Other(format!("cannot serialize track: {e}")))?;
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO play_history
                 (server_url, user_id, played_at, listened_ms, item_id, track)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                server,
                user,
                played_at,
                listened_ms as i64,
                track.item_id,
                json
            ],
        )
    })?;
    Ok(true)
}

fn entry(id: i64, played_at: i64, listened_ms: i64, json: &str) -> Option<HistoryEntry> {
    // A row this build cannot read is skipped, not an error: the page around
    // it is still worth showing.
    let track = serde_json::from_str(json).ok()?;
    Some(HistoryEntry {
        id,
        played_at,
        listened_ms: listened_ms.max(0) as u64,
        track,
    })
}

/// Newest first, `limit` rows older than the row `before` (from the top when
/// `None`). Fewer only at the end of the history: a row this build cannot
/// read is skipped and the page filled up from further back, because the
/// timeline takes a short page for the end.
pub fn page(store: &Store, before: Option<i64>, limit: u32) -> AppResult<Vec<HistoryEntry>> {
    let Some(user) = account(store) else {
        return Ok(Vec::new());
    };
    let limit = limit.clamp(1, MAX_PAGE) as usize;
    let mut out = Vec::new();
    let mut cursor = before.unwrap_or(i64::MAX);
    loop {
        let wanted = limit - out.len();
        let rows = store.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, played_at, listened_ms, track FROM play_history
                 WHERE user_id = ?1 AND id < ?2
                 ORDER BY id DESC LIMIT ?3",
            )?;
            let rows = statement
                .query_map(params![user, cursor, wanted as i64], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        let scanned = rows.len();
        if let Some((id, ..)) = rows.last() {
            cursor = *id;
        }
        out.extend(
            rows.into_iter()
                .filter_map(|(id, played_at, listened_ms, json)| {
                    entry(id, played_at, listened_ms, &json)
                }),
        );
        if scanned < wanted || out.len() >= limit {
            return Ok(out);
        }
    }
}

/// The local year of a Unix-ms instant, by SQLite's idea of the time zone —
/// the one everything here is counted in.
fn local_year(conn: &rusqlite::Connection, at_ms: i64) -> rusqlite::Result<i32> {
    conn.query_row(
        "SELECT CAST(strftime('%Y', ?1 / 1000, 'unixepoch', 'localtime') AS INTEGER)",
        [at_ms],
        |row| row.get(0),
    )
}

/// Local midnight of 1 January of `year` and of the year after, as Unix ms:
/// a range the time index can serve, where comparing every row's local year
/// would have to convert every play the account ever recorded. `None` for a
/// year SQLite cannot spell.
fn year_bounds(conn: &rusqlite::Connection, year: i32) -> rusqlite::Result<Option<(i64, i64)>> {
    if !(1..=9998).contains(&year) {
        return Ok(None);
    }
    let new_year = |year: i32| -> rusqlite::Result<Option<i64>> {
        conn.query_row(
            "SELECT CAST(strftime('%s', printf('%04d-01-01 00:00:00', ?1), 'utc') AS INTEGER)
                    * 1000",
            [year],
            |row| row.get(0),
        )
    };
    Ok(new_year(year)?.zip(new_year(year + 1)?))
}

pub fn years(store: &Store) -> AppResult<HistoryYears> {
    let none = HistoryYears {
        years: Vec::new(),
        recorded_since: None,
    };
    let Some(user) = account(store) else {
        return Ok(none);
    };
    store.with_conn(|conn| {
        // The first and the last play through the time index, then each year
        // between them asked for a play.
        let (first, last): (Option<i64>, Option<i64>) = conn.query_row(
            "SELECT (SELECT MIN(played_at) FROM play_history WHERE user_id = ?1),
                    (SELECT MAX(played_at) FROM play_history WHERE user_id = ?1)",
            params![user],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (Some(first), Some(last)) = (first, last) else {
            return Ok(none);
        };
        let mut years = Vec::new();
        for year in (local_year(conn, first)?..=local_year(conn, last)?).rev() {
            let Some((start, end)) = year_bounds(conn, year)? else {
                continue;
            };
            let played: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM play_history
                                WHERE user_id = ?1 AND played_at >= ?2 AND played_at < ?3)",
                params![user, start, end],
                |row| row.get(0),
            )?;
            if played {
                years.push(year);
            }
        }
        Ok(HistoryYears {
            years,
            recorded_since: Some(first),
        })
    })
}

/// Add up one year, in local time.
pub fn year(store: &Store, year: i32) -> AppResult<YearReview> {
    let rows = match account(store) {
        None => Vec::new(),
        Some(user) => store.with_conn(|conn| {
            let Some((start, end)) = year_bounds(conn, year)? else {
                return Ok(Vec::new());
            };
            let mut statement = conn.prepare(
                "SELECT id, played_at, listened_ms, track,
                        CAST(strftime('%m', played_at / 1000, 'unixepoch', 'localtime')
                             AS INTEGER)
                 FROM play_history
                 WHERE user_id = ?1 AND played_at >= ?2 AND played_at < ?3
                 ORDER BY id",
            )?;
            let rows = statement
                .query_map(params![user, start, end], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u32>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?,
    };
    // Read once the store is free again: the player writes to it as well.
    let plays: Vec<(HistoryEntry, u32)> = rows
        .into_iter()
        .filter_map(|(id, played_at, listened_ms, json, month)| {
            entry(id, played_at, listened_ms, &json).map(|e| (e, month))
        })
        .collect();
    Ok(review(year, &plays))
}

/// Between a main artist and a guest, the way tags write it. Jellyfin keeps
/// "Daft Punk feat. Pharrell Williams" as one artist unless the library is
/// set up to split it.
const FEATURING: [&str; 10] = [
    " feat. ",
    " feat ",
    " ft. ",
    " ft ",
    " featuring ",
    " with ",
    " (feat. ",
    " (ft. ",
    " (featuring ",
    " (with ",
];
/// Between artists of equal standing: "Simon & Garfunkel".
const AND: [&str; 4] = [" & ", ", ", " and ", " vs. "];

/// `text` cut at each of `markers`, ignoring ASCII case. The markers are
/// ASCII from end to end, so a cut never falls inside a character.
fn split_at_markers<'a>(text: &'a str, markers: &[&str]) -> Vec<&'a str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < bytes.len() {
        let marker = markers.iter().find(|marker| {
            bytes[at..]
                .get(..marker.len())
                .is_some_and(|window| window.eq_ignore_ascii_case(marker.as_bytes()))
        });
        match marker {
            Some(marker) => {
                parts.push(&text[start..at]);
                at += marker.len();
                start = at;
            }
            None => at += 1,
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A name a played track gives for its album's artist.
struct Name {
    /// Lower-cased, the way names are compared.
    key: String,
    /// As the track spells it.
    shown: String,
    /// How far it was cut out of a credit: 0 the credit as it is, 1 a part
    /// of it at a "feat.", 2 a part of that at an "&" or a comma.
    depth: u8,
}

fn add_name(names: &mut Vec<Name>, name: &str, depth: u8) {
    let name = name.trim();
    // What a "(feat. …" cut leaves at the end of the guest.
    let shown = if depth > 0 {
        name.trim_end_matches(')').trim_end()
    } else {
        name
    };
    if shown.is_empty() {
        return;
    }
    let key = shown.to_lowercase();
    match names.iter_mut().find(|n| n.key == key) {
        Some(known) => known.depth = known.depth.min(depth),
        None => names.push(Name {
            key,
            shown: shown.to_string(),
            depth,
        }),
    }
}

/// A credit and the names cut out of it: "Daft Punk feat. Pharrell Williams
/// & Nile Rodgers" also names Daft Punk, and Pharrell Williams.
fn add_credit(names: &mut Vec<Name>, credit: &str) {
    add_name(names, credit, 0);
    let mut parts = split_at_markers(credit, &FEATURING);
    if parts.len() > 1 {
        for part in &parts {
            add_name(names, part, 1);
        }
        // The whole credit is cut at an "&" too: a name with a marker in
        // it ("Little Feat & Friends") must not be cut the wrong way only.
        parts.push(credit);
    }
    for part in parts {
        let pieces = split_at_markers(part, &AND);
        if pieces.len() > 1 {
            for piece in pieces {
                add_name(names, piece, 2);
            }
        }
    }
}

/// Whom a play names for its album: the album artist the track is tagged
/// with, or else its own artists — the linkable ones, or the shown name of a
/// track without any.
fn album_names(track: &HistoryTrack) -> Vec<Name> {
    let mut names = Vec::new();
    let album_artist = track
        .album_artist
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    match album_artist {
        Some(album_artist) => add_credit(&mut names, album_artist),
        None if track.artists.is_empty() => add_credit(&mut names, &track.artist),
        None => {
            for artist in &track.artists {
                add_credit(&mut names, &artist.name);
            }
        }
    }
    names
}

/// Whom an album is credited to, from what its played tracks name (newest
/// first): the names more than half of them share, and of those the ones
/// nearest a whole credit ("Simon & Garfunkel", not "Simon"). A track's own
/// artist is not its album's — a guest on one track, a compilation with
/// another artist on every one — so it takes most of the tracks, not all of
/// them and not the first. `(artist, various artists)`: nobody in common is a
/// compilation; nobody named at all, no credit.
fn shared_credit(tracks: &[&[Name]]) -> (String, bool) {
    let tracks: Vec<&[Name]> = tracks
        .iter()
        .copied()
        .filter(|names| !names.is_empty())
        .collect();
    // Per name, in the order first met: how many tracks name it, the depth
    // nearest a whole credit it came at, and how the newest track spells it.
    let mut tally: Vec<(&str, usize, u8, &str)> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for names in &tracks {
        for name in names.iter() {
            match at.get(name.key.as_str()) {
                Some(&i) => {
                    tally[i].1 += 1;
                    tally[i].2 = tally[i].2.min(name.depth);
                }
                None => {
                    at.insert(&name.key, tally.len());
                    tally.push((&name.key, 1, name.depth, &name.shown));
                }
            }
        }
    }
    let Some(most) = tally.iter().map(|t| t.1).max() else {
        return (String::new(), false);
    };
    if most * 2 <= tracks.len() {
        return (String::new(), true);
    }
    let nearest = tally
        .iter()
        .filter(|t| t.1 == most)
        .map(|t| t.2)
        .min()
        .unwrap_or(0);
    let artist = tally
        .iter()
        .filter(|t| t.1 == most && t.2 == nearest)
        .map(|t| t.3)
        .collect::<Vec<_>>()
        .join(", ");
    (artist, false)
}

/// An album being added up.
struct AlbumTally {
    plays: AlbumPlays,
    /// Per track played from it: where its newest play is in the year, and
    /// whom that play names for the album.
    tracks: HashMap<String, (usize, Vec<Name>)>,
}

impl AlbumTally {
    fn into_plays(self) -> AlbumPlays {
        let mut tracks: Vec<(usize, Vec<Name>)> = self.tracks.into_values().collect();
        // Newest first: the newest play spells the names.
        tracks.sort_by_key(|(place, _)| std::cmp::Reverse(*place));
        let names: Vec<&[Name]> = tracks.iter().map(|(_, names)| names.as_slice()).collect();
        let (artist, various_artists) = shared_credit(&names);
        AlbumPlays {
            artist,
            various_artists,
            ..self.plays
        }
    }
}

/// The arithmetic of [`year`], apart from the database. `month` is 1–12.
fn review(year: i32, plays: &[(HistoryEntry, u32)]) -> YearReview {
    let mut months = [0i64; 12];
    let mut listened_ms = 0u64;
    // Per track: the newest copy of its metadata (a retagged track shows as it
    // is now) and how often it was played.
    let mut by_track: HashMap<&str, (&HistoryTrack, i64)> = HashMap::new();
    let mut by_album: HashMap<String, AlbumTally> = HashMap::new();
    for (place, (entry, month)) in plays.iter().enumerate() {
        if let Some(slot) = months.get_mut((*month as usize).wrapping_sub(1)) {
            *slot += 1;
        }
        listened_ms = listened_ms.saturating_add(entry.listened_ms);
        let track = &entry.track;
        let slot = by_track.entry(&track.item_id).or_insert((track, 0));
        slot.0 = track;
        slot.1 += 1;

        // An album without an id is told apart by its name and artist, the
        // way it is shown.
        let key = match &track.album_id {
            Some(id) => id.clone(),
            None if track.album.trim().is_empty() => continue,
            None => format!(
                "\u{1}{}\u{1}{}",
                track.album.trim().to_lowercase(),
                track
                    .album_artist
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(&track.artist)
                    .trim()
                    .to_lowercase()
            ),
        };
        let album = by_album.entry(key).or_insert_with(|| AlbumTally {
            plays: AlbumPlays {
                album_id: track.album_id.clone(),
                name: String::new(),
                artist: String::new(),
                various_artists: false,
                image_item_id: None,
                image_tag: None,
                image_blur_hash: None,
                plays: 0,
            },
            tracks: HashMap::new(),
        });
        album.plays.plays += 1;
        // Rows come oldest first: the newest play names the album, as it is
        // tagged now, and says whom its track credits.
        album.plays.name = track.album.clone();
        album
            .tracks
            .insert(track.item_id.clone(), (place, album_names(track)));
        // The album's own picture where the track has one of it; a track
        // without art leaves what an earlier one found.
        if track.image_item_id.is_some() {
            album.plays.image_item_id = track.image_item_id.clone();
            album.plays.image_tag = track.image_tag.clone();
            album.plays.image_blur_hash = track.image_blur_hash.clone();
        }
    }

    // The artist chart already knows how to credit a duet to both artists and
    // a track without linkable artists to its shown name; feed it the counts.
    let counted: Vec<TrackDto> = by_track
        .values()
        .map(|(track, plays)| TrackDto {
            id: track.item_id.clone(),
            name: track.name.clone(),
            artist: track.artist.clone(),
            artists: track.artists.clone(),
            play_count: i32::try_from(*plays).unwrap_or(i32::MAX),
            ..TrackDto::default()
        })
        .collect();
    let all_artists = top_artists(&counted, usize::MAX);

    let mut top_tracks: Vec<TrackPlays> = by_track
        .into_values()
        .map(|(track, plays)| TrackPlays {
            track: track.clone(),
            plays,
        })
        .collect();
    // Ties by name, then id, so the order does not change between two visits.
    top_tracks.sort_by(|a, b| {
        b.plays
            .cmp(&a.plays)
            .then_with(|| a.track.name.cmp(&b.track.name))
            .then_with(|| a.track.item_id.cmp(&b.track.item_id))
    });
    let tracks = top_tracks.len() as i64;
    top_tracks.truncate(TOP_TRACKS);

    let mut top_albums: Vec<AlbumPlays> =
        by_album.into_values().map(AlbumTally::into_plays).collect();
    top_albums.sort_by(|a, b| {
        b.plays
            .cmp(&a.plays)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.album_id.cmp(&b.album_id))
    });
    top_albums.truncate(TOP_ALBUMS);

    YearReview {
        year,
        plays: plays.len() as i64,
        listened_ms,
        tracks,
        artists: all_artists.len() as i64,
        top_tracks,
        top_artists: all_artists.into_iter().take(TOP_ARTISTS).collect(),
        top_albums,
        months,
        first_played_at: plays.iter().map(|(e, _)| e.played_at).min(),
        last_played_at: plays.iter().map(|(e, _)| e.played_at).max(),
    }
}

/// Forget the history of the account signed in now, under whatever address
/// it was played. Other accounts keep theirs.
pub fn clear(store: &Store) -> AppResult<usize> {
    let Some(user) = account(store) else {
        return Ok(0);
    };
    store.with_conn(|conn| {
        conn.execute("DELETE FROM play_history WHERE user_id = ?1", params![user])
    })
}

/// Forget one listen.
pub fn delete(store: &Store, id: i64) -> AppResult<bool> {
    let Some(user) = account(store) else {
        return Ok(false);
    };
    let removed = store.with_conn(|conn| {
        conn.execute(
            "DELETE FROM play_history WHERE id = ?1 AND user_id = ?2",
            params![id, user],
        )
    })?;
    Ok(removed > 0)
}

/// The addresses other than `server` the account `user` has played under.
/// A Jellyfin user id is minted by one server, so they are all that server,
/// reached another way — a new IP, a host name, https through a proxy.
pub fn other_addresses(store: &Store, user: &str, server: &str) -> AppResult<Vec<String>> {
    store.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT DISTINCT server_url FROM play_history
             WHERE user_id = ?1 AND server_url <> ?2 AND server_url <> ''",
        )?;
        let addresses = statement
            .query_map(params![user, server], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(addresses)
    })
}

/// Whether another account has played on the server `user` is signed in to:
/// under `server`, or under an address `user` has played under.
pub fn shared_with_others(store: &Store, user: &str, server: &str) -> AppResult<bool> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM play_history
                            WHERE user_id <> ?1
                              AND (server_url = ?2
                                   OR server_url IN (SELECT server_url FROM play_history
                                                     WHERE user_id = ?1)))",
            params![user, server],
            |row| row.get(0),
        )
    })
}

/// Which of `item_ids` the account `user` has played, under any address.
pub fn played_by(store: &Store, user: &str, item_ids: &[String]) -> AppResult<HashSet<String>> {
    let mut played = HashSet::new();
    // Well under the number of parameters SQLite takes in one statement.
    for chunk in item_ids.chunks(500) {
        let sql = format!(
            "SELECT DISTINCT item_id FROM play_history WHERE user_id = ? AND item_id IN ({})",
            vec!["?"; chunk.len()].join(", ")
        );
        let found = store.with_conn(|conn| {
            let mut statement = conn.prepare(&sql)?;
            let values = std::iter::once(user).chain(chunk.iter().map(String::as_str));
            let found = statement
                .query_map(rusqlite::params_from_iter(values), |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(found)
        })?;
        played.extend(found);
    }
    Ok(played)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A store in its own temp directory, closed before the directory goes:
    /// Windows refuses to delete an open database file.
    struct TempStore {
        store: Option<Store>,
        dir: PathBuf,
    }

    impl std::ops::Deref for TempStore {
        type Target = Store;
        fn deref(&self) -> &Store {
            self.store.as_ref().expect("open until drop")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            self.store.take();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn signed_in() -> TempStore {
        let dir =
            std::env::temp_dir().join(format!("sonarune-history-test-{}", uuid::Uuid::new_v4()));
        let store = TempStore {
            store: Some(Store::open(&dir).expect("open test store")),
            dir,
        };
        store.set(keys::SERVER_URL, "https://music.home").unwrap();
        store.set(keys::USER_ID, "u1").unwrap();
        store
    }

    fn track(id: &str, artist: &str, album: Option<&str>) -> HistoryTrack {
        HistoryTrack {
            item_id: id.into(),
            name: format!("Track {id}"),
            artist: artist.into(),
            album: album.map(|a| format!("Album {a}")).unwrap_or_default(),
            album_id: album.map(str::to_string),
            album_artist: None,
            artists: vec![ArtistRef {
                id: artist.to_lowercase(),
                name: artist.into(),
            }],
            duration_ms: 200_000,
            image_item_id: album.map(str::to_string),
            image_tag: None,
            image_blur_hash: None,
        }
    }

    /// A local time as Unix ms — local by SQLite's idea of the time zone,
    /// the one the years and months are counted in, so the tests hold in
    /// every zone (noon UTC is already tomorrow in New Zealand).
    fn local(year: i32, month: u32, day: u32, time: &str) -> i64 {
        rusqlite::Connection::open_in_memory()
            .unwrap()
            .query_row(
                "SELECT CAST(strftime('%s', printf('%04d-%02d-%02d %s', ?1, ?2, ?3, ?4), 'utc')
                             AS INTEGER) * 1000",
                params![year, month, day, time],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Noon on a day of 2026, local time.
    fn day(month: u32, day: u32) -> i64 {
        local(2026, month, day, "12:00:00")
    }

    #[test]
    fn pages_go_back_in_time_and_end() {
        let store = signed_in();
        for i in 0..5 {
            record_at(
                &store,
                &track(&i.to_string(), "A", Some("x")),
                1_000,
                day(3, 1 + i),
            )
            .unwrap();
        }
        let first = page(&store, None, 2).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|e| e.track.item_id.as_str())
                .collect::<Vec<_>>(),
            ["4", "3"]
        );
        let second = page(&store, Some(first[1].id), 2).unwrap();
        assert_eq!(
            second
                .iter()
                .map(|e| e.track.item_id.as_str())
                .collect::<Vec<_>>(),
            ["2", "1"]
        );
        let last = page(&store, Some(second[1].id), 2).unwrap();
        assert_eq!(last.len(), 1);
        assert!(page(&store, Some(last[0].id), 2).unwrap().is_empty());
    }

    #[test]
    fn each_account_sees_only_its_own_history() {
        let store = signed_in();
        record_at(&store, &track("a", "A", None), 1_000, day(1, 5)).unwrap();
        store.set(keys::USER_ID, "u2").unwrap();
        assert!(page(&store, None, 50).unwrap().is_empty());
        assert!(years(&store).unwrap().years.is_empty());
        record_at(&store, &track("b", "B", None), 1_000, day(1, 6)).unwrap();
        assert_eq!(clear(&store).unwrap(), 1);

        store.set(keys::USER_ID, "u1").unwrap();
        let rows = page(&store, None, 50).unwrap();
        assert_eq!(rows.len(), 1, "clearing u2 left u1 alone");
        assert!(!delete(&store, rows[0].id + 1).unwrap());
        assert!(delete(&store, rows[0].id).unwrap());
        assert!(page(&store, None, 50).unwrap().is_empty());
    }

    #[test]
    fn the_same_account_under_a_new_address_keeps_its_history() {
        let store = signed_in();
        record_at(&store, &track("a", "A", None), 1_000, day(1, 5)).unwrap();
        // The server moved behind a reverse proxy; the account is the same.
        store
            .set(keys::SERVER_URL, "https://music.example.org")
            .unwrap();
        record_at(&store, &track("b", "A", None), 1_000, day(1, 6)).unwrap();
        assert_eq!(page(&store, None, 50).unwrap().len(), 2);
        assert_eq!(year(&store, 2026).unwrap().plays, 2);
        assert_eq!(years(&store).unwrap().recorded_since, Some(day(1, 5)));
        assert_eq!(clear(&store).unwrap(), 2, "every play of the account");
    }

    #[test]
    fn an_unreadable_row_does_not_end_the_timeline() {
        let store = signed_in();
        let unreadable = |store: &Store| {
            store
                .with_conn(|conn| {
                    conn.execute(
                        "INSERT INTO play_history
                             (server_url, user_id, played_at, listened_ms, item_id, track)
                         VALUES ('https://music.home', 'u1', 0, 0, 'x', 'not json')",
                        [],
                    )
                })
                .unwrap();
        };
        unreadable(&store);
        for i in 0..3 {
            record_at(
                &store,
                &track(&i.to_string(), "A", None),
                1_000,
                day(2, 1 + i),
            )
            .unwrap();
        }
        unreadable(&store);
        for i in 3..5 {
            record_at(
                &store,
                &track(&i.to_string(), "A", None),
                1_000,
                day(2, 1 + i),
            )
            .unwrap();
        }
        let ids = |rows: &[HistoryEntry]| {
            rows.iter()
                .map(|e| e.track.item_id.clone())
                .collect::<Vec<_>>()
        };
        // A full page around the row it skips: only a short one is the end.
        let first = page(&store, None, 3).unwrap();
        assert_eq!(ids(&first), ["4", "3", "2"]);
        // Nothing but an unreadable row further back: the end, not a loop.
        let second = page(&store, Some(first[2].id), 3).unwrap();
        assert_eq!(ids(&second), ["1", "0"]);
        assert!(page(&store, Some(second[1].id), 3).unwrap().is_empty());
    }

    #[test]
    fn a_year_starts_and_ends_at_local_midnight() {
        let store = signed_in();
        for (id, at) in [
            ("old", local(2025, 12, 31, "23:59:00")),
            ("new", local(2026, 1, 1, "00:01:00")),
            ("last", local(2026, 12, 31, "23:59:00")),
            ("next", local(2027, 1, 1, "00:01:00")),
        ] {
            record_at(&store, &track(id, "A", None), 1_000, at).unwrap();
        }
        let review = year(&store, 2026).unwrap();
        assert_eq!(review.plays, 2);
        assert_eq!((review.months[0], review.months[11]), (1, 1));
        assert_eq!(years(&store).unwrap().years, [2027, 2026, 2025]);
        assert_eq!(year(&store, 0).unwrap().plays, 0, "no such year");
    }

    /// A play of `item` on the album with id `album`, credited to `artists`
    /// (each a linkable artist, as Jellyfin lists them) and, when given, to
    /// the album artist the track is tagged with.
    fn album_play(
        id: i64,
        item: &str,
        artists: &[&str],
        album: &str,
        album_artist: Option<&str>,
    ) -> (HistoryEntry, u32) {
        let entry = HistoryEntry {
            id,
            played_at: day(3, 1),
            listened_ms: 1,
            track: HistoryTrack {
                item_id: item.into(),
                name: item.into(),
                artist: artists.join(", "),
                album: format!("Album {album}"),
                album_id: Some(album.into()),
                album_artist: album_artist.map(str::to_string),
                artists: artists
                    .iter()
                    .map(|a| ArtistRef {
                        id: a.to_lowercase(),
                        name: a.to_string(),
                    })
                    .collect(),
                duration_ms: 200_000,
                image_item_id: None,
                image_tag: None,
                image_blur_hash: None,
            },
        };
        (entry, 3)
    }

    /// (artist, various artists) of each album of `plays`, by album id.
    fn credits(plays: &[(HistoryEntry, u32)]) -> HashMap<String, (String, bool)> {
        review(2026, plays)
            .top_albums
            .into_iter()
            .map(|a| (a.album_id.unwrap(), (a.artist, a.various_artists)))
            .collect()
    }

    #[test]
    fn an_album_is_credited_to_the_artists_its_tracks_share() {
        let mut plays = vec![
            // A compilation: every track by someone else.
            album_play(1, "c1", &["Dua Lipa"], "hits", None),
            album_play(2, "c2", &["Queen"], "hits", None),
            // The first track played has a guest; the album is still one
            // artist's, and since retagged.
            album_play(3, "r1", &["Daft Punk", "Pharrell Williams"], "ram", None),
            album_play(4, "r2", &["Daft Punk"], "ram", None),
        ];
        plays[2].0.track.album = "RAM".into();
        let review = review(2026, &plays);
        let album = |id: &str| {
            review
                .top_albums
                .iter()
                .find(|a| a.album_id.as_deref() == Some(id))
                .unwrap()
                .clone()
        };
        let hits = album("hits");
        assert!(hits.various_artists && hits.artist.is_empty(), "{hits:?}");
        let ram = album("ram");
        assert_eq!(
            (ram.artist.as_str(), ram.name.as_str(), ram.various_artists),
            ("Daft Punk", "Album ram", false)
        );
    }

    #[test]
    fn a_guest_in_one_credit_does_not_make_a_compilation() {
        // Jellyfin keeps an untouched "feat." tag as one artist.
        let plays = [
            album_play(
                1,
                "r1",
                &["Daft Punk feat. Pharrell Williams & Nile Rodgers"],
                "ram",
                None,
            ),
            album_play(2, "r2", &["Daft Punk"], "ram", None),
            album_play(3, "v1", &["Drake"], "views", None),
            album_play(4, "v2", &["Drake FEAT. Rihanna"], "views", None),
            album_play(5, "v3", &["Drake (feat. Kyla & Wizkid)"], "views", None),
            // A band with an "&" in its name stays the band, one with a
            // marker in its name too.
            album_play(6, "b1", &["Simon & Garfunkel"], "bridge", None),
            album_play(7, "b2", &["Simon & Garfunkel"], "bridge", None),
            album_play(11, "l1", &["Little Feat"], "waiting", None),
            album_play(12, "l2", &["Little Feat & Friends"], "waiting", None),
            // Two artists on every track: both.
            album_play(8, "w1", &["One", "Two"], "duets", None),
            album_play(9, "w2", &["One", "Two"], "duets", None),
            // Only the guest track of an album played: the whole credit.
            album_play(10, "g1", &["Beyoncé feat. JAY-Z"], "single", None),
        ];
        let credits = credits(&plays);
        let artist = |id: &str| credits[id].clone();
        assert_eq!(artist("ram"), ("Daft Punk".to_string(), false));
        assert_eq!(artist("views"), ("Drake".to_string(), false));
        assert_eq!(artist("bridge"), ("Simon & Garfunkel".to_string(), false));
        assert_eq!(artist("waiting"), ("Little Feat".to_string(), false));
        assert_eq!(artist("duets"), ("One, Two".to_string(), false));
        assert_eq!(artist("single"), ("Beyoncé feat. JAY-Z".to_string(), false));
    }

    #[test]
    fn a_compilation_is_told_by_its_tracks_not_by_its_plays() {
        // One of its tracks played over and over is still one of two.
        let mut plays: Vec<_> = (1..=5)
            .map(|id| album_play(id, "c1", &["Dua Lipa"], "hits", None))
            .collect();
        plays.push(album_play(6, "c2", &["Queen"], "hits", None));
        assert_eq!(credits(&plays)["hits"], (String::new(), true));
        // Most of its tracks by one artist: that artist's.
        plays.push(album_play(
            7,
            "c3",
            &["Dua Lipa feat. DaBaby"],
            "hits",
            None,
        ));
        assert_eq!(credits(&plays)["hits"], ("Dua Lipa".to_string(), false));
    }

    #[test]
    fn an_album_is_credited_to_the_album_artist_its_tracks_are_tagged_with() {
        let plays = [
            album_play(1, "c1", &["Dua Lipa"], "hits", Some("Various Artists")),
            album_play(2, "c2", &["Queen"], "hits", Some("Various Artists")),
            album_play(3, "c3", &["Dua Lipa"], "hits", Some("Various Artists")),
            // Only the guest track played.
            album_play(
                4,
                "r1",
                &["Daft Punk feat. Pharrell Williams & Nile Rodgers"],
                "ram",
                Some("Daft Punk"),
            ),
            // A row from before the album artist was kept, next to one after.
            album_play(5, "s1", &["Sade"], "love", None),
            album_play(6, "s2", &["Sade"], "love", Some("Sade")),
        ];
        let credits = credits(&plays);
        assert_eq!(credits["hits"], ("Various Artists".to_string(), false));
        assert_eq!(credits["ram"], ("Daft Punk".to_string(), false));
        assert_eq!(credits["love"], ("Sade".to_string(), false));
    }

    #[test]
    fn credits_are_cut_at_their_markers_whatever_the_case() {
        assert_eq!(
            split_at_markers("Beyoncé FEAT. Jay-Z & Ásgeir", &FEATURING),
            ["Beyoncé", "Jay-Z & Ásgeir"]
        );
        assert_eq!(
            split_at_markers("Jay-Z & Ásgeir", &AND),
            ["Jay-Z", "Ásgeir"]
        );
        assert_eq!(
            split_at_markers("Featherstone", &FEATURING),
            ["Featherstone"]
        );
        assert_eq!(split_at_markers("", &AND), [""]);
    }

    #[test]
    fn the_plays_of_an_account_tie_its_addresses_together() {
        let store = signed_in();
        record_at(&store, &track("a", "A", None), 1_000, day(1, 5)).unwrap();
        store
            .set(keys::SERVER_URL, "https://music.example.org")
            .unwrap();
        record_at(&store, &track("b", "A", None), 1_000, day(1, 6)).unwrap();
        assert_eq!(
            other_addresses(&store, "u1", "https://music.example.org").unwrap(),
            ["https://music.home"]
        );
        assert!(other_addresses(&store, "u2", "https://music.example.org")
            .unwrap()
            .is_empty());
        assert_eq!(
            played_by(&store, "u1", &["a".into(), "b".into(), "c".into()]).unwrap(),
            HashSet::from(["a".to_string(), "b".to_string()])
        );
        assert!(played_by(&store, "u2", &["a".into()]).unwrap().is_empty());

        // Alone on its server, under both of its addresses.
        assert!(!shared_with_others(&store, "u1", "https://music.example.org").unwrap());
        // A second account plays on the server under its old address.
        store.set(keys::SERVER_URL, "https://music.home").unwrap();
        store.set(keys::USER_ID, "kid").unwrap();
        record_at(&store, &track("k", "K", None), 1_000, day(1, 7)).unwrap();
        assert!(shared_with_others(&store, "u1", "https://music.example.org").unwrap());
        assert!(shared_with_others(&store, "kid", "https://music.home").unwrap());
        // An account on another server shares nothing with them.
        assert!(!shared_with_others(&store, "far", "https://far.away").unwrap());
    }

    #[test]
    fn nothing_is_recorded_while_signed_out() {
        let store = signed_in();
        store.delete(keys::USER_ID).unwrap();
        assert!(!record_at(&store, &track("a", "A", None), 1_000, day(1, 1)).unwrap());
        assert!(page(&store, None, 50).unwrap().is_empty());
    }

    #[test]
    fn a_year_adds_up_plays_tracks_artists_albums_and_months() {
        let store = signed_in();
        let plays = [
            ("1", "Alpha", Some("x"), day(1, 10)),
            ("1", "Alpha", Some("x"), day(1, 11)),
            ("2", "Alpha", Some("x"), day(2, 1)),
            ("3", "Beta", Some("y"), day(12, 30)),
            ("4", "Beta", None, day(12, 31)),
        ];
        for (id, artist, album, at) in plays {
            record_at(&store, &track(id, artist, album), 60_000, at).unwrap();
        }
        let review = year(&store, 2026).unwrap();
        assert_eq!(review.plays, 5);
        assert_eq!(review.listened_ms, 300_000);
        assert_eq!(review.tracks, 4);
        assert_eq!(review.artists, 2);
        assert_eq!(review.months[0], 2);
        assert_eq!(review.months[1], 1);
        assert_eq!(review.months[11], 2);
        assert_eq!(review.top_tracks[0].track.item_id, "1");
        assert_eq!(review.top_tracks[0].plays, 2);
        assert_eq!(review.top_artists[0].name, "Alpha");
        assert_eq!(review.top_artists[0].plays, 3);
        // The track without an album is in no album.
        assert_eq!(
            review
                .top_albums
                .iter()
                .map(|a| (a.album_id.as_deref(), a.plays))
                .collect::<Vec<_>>(),
            [(Some("x"), 3), (Some("y"), 1)]
        );
        assert_eq!(review.first_played_at, Some(day(1, 10)));
        assert_eq!(review.last_played_at, Some(day(12, 31)));

        assert_eq!(years(&store).unwrap().years, [2026]);
        assert_eq!(years(&store).unwrap().recorded_since, Some(day(1, 10)));
        let empty = year(&store, 2025).unwrap();
        assert_eq!((empty.plays, empty.top_tracks.len()), (0, 0));
    }

    #[test]
    fn a_duet_counts_for_both_artists() {
        let mut duet = track("d", "One & Two", Some("z"));
        duet.artists = vec![
            ArtistRef {
                id: "1".into(),
                name: "One".into(),
            },
            ArtistRef {
                id: "2".into(),
                name: "Two".into(),
            },
        ];
        let entry = HistoryEntry {
            id: 1,
            played_at: day(5, 5),
            listened_ms: 1,
            track: duet,
        };
        let review = review(2026, &[(entry, 5)]);
        assert_eq!(review.artists, 2);
        assert!(review.top_artists.iter().all(|a| a.plays == 1));
    }

    #[test]
    fn an_unreadable_row_is_skipped() {
        let store = signed_in();
        record_at(&store, &track("a", "A", None), 1_000, day(1, 1)).unwrap();
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO play_history
                         (server_url, user_id, played_at, listened_ms, item_id, track)
                     VALUES ('https://music.home', 'u1', 0, 0, 'b', 'not json')",
                    [],
                )
            })
            .unwrap();
        let rows = page(&store, None, 50).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].track.item_id, "a");
    }
}
