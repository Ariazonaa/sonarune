//! Play history, kept by the client itself.
//!
//! Jellyfin remembers per track how often it was played and when it was last
//! played, never when each play happened. A timeline and a review of a year
//! need the plays themselves, so the player writes one row per listen here:
//! a track counts by the ListenBrainz rule (`scrobble::should_scrobble`),
//! whether or not ListenBrainz is set up. The rows never leave the machine.
//!
//! Every row belongs to the server and account it was played on, like the
//! persisted queue: signing in elsewhere shows that account's history, not a
//! mix of both.

use crate::api::types::{ArtistRef, TrackDto};
use crate::commands_library::{top_artists, ArtistPlays};
use crate::error::AppResult;
use crate::player::QueueTrack;
use crate::store::{keys, Store};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub artist: String,
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

/// The table. Created with the store (`Store::open`).
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
    CREATE INDEX IF NOT EXISTS play_history_owner
        ON play_history (server_url, user_id, id);";

/// Server and account the history is kept under right now; `None` while
/// signed out, when there is nothing to record or show.
fn owner(store: &Store) -> Option<(String, String)> {
    Some((store.get(keys::SERVER_URL)?, store.get(keys::USER_ID)?))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Write a listen that has just ended. Returns whether a row was written.
pub fn record(store: &Store, track: &QueueTrack, listened_ms: u64) -> AppResult<bool> {
    record_at(store, &HistoryTrack::from(track), listened_ms, now_ms())
}

fn record_at(
    store: &Store,
    track: &HistoryTrack,
    listened_ms: u64,
    played_at: i64,
) -> AppResult<bool> {
    let Some((server, user)) = owner(store) else {
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
/// `None`).
pub fn page(store: &Store, before: Option<i64>, limit: u32) -> AppResult<Vec<HistoryEntry>> {
    let Some((server, user)) = owner(store) else {
        return Ok(Vec::new());
    };
    let limit = limit.clamp(1, MAX_PAGE);
    store.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, played_at, listened_ms, track FROM play_history
             WHERE server_url = ?1 AND user_id = ?2 AND id < ?3
             ORDER BY id DESC LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![server, user, before.unwrap_or(i64::MAX), limit],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (id, played_at, listened_ms, json) = row?;
            out.extend(entry(id, played_at, listened_ms, &json));
        }
        Ok(out)
    })
}

pub fn years(store: &Store) -> AppResult<HistoryYears> {
    let Some((server, user)) = owner(store) else {
        return Ok(HistoryYears {
            years: Vec::new(),
            recorded_since: None,
        });
    };
    store.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT DISTINCT CAST(strftime('%Y', played_at / 1000, 'unixepoch', 'localtime')
                                  AS INTEGER) AS year
             FROM play_history WHERE server_url = ?1 AND user_id = ?2
             ORDER BY year DESC",
        )?;
        let years = statement
            .query_map(params![server, user], |row| row.get::<_, i32>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let recorded_since = conn.query_row(
            "SELECT MIN(played_at) FROM play_history WHERE server_url = ?1 AND user_id = ?2",
            params![server, user],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(HistoryYears {
            years,
            recorded_since,
        })
    })
}

/// Add up one year, in local time.
pub fn year(store: &Store, year: i32) -> AppResult<YearReview> {
    let plays = match owner(store) {
        None => Vec::new(),
        Some((server, user)) => store.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, played_at, listened_ms, track,
                        CAST(strftime('%m', played_at / 1000, 'unixepoch', 'localtime')
                             AS INTEGER)
                 FROM play_history
                 WHERE server_url = ?1 AND user_id = ?2
                   AND CAST(strftime('%Y', played_at / 1000, 'unixepoch', 'localtime')
                            AS INTEGER) = ?3
                 ORDER BY id",
            )?;
            let rows = statement.query_map(params![server, user, year], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, played_at, listened_ms, json, month) = row?;
                out.extend(entry(id, played_at, listened_ms, &json).map(|e| (e, month)));
            }
            Ok(out)
        })?,
    };
    Ok(review(year, &plays))
}

/// The arithmetic of [`year`], apart from the database. `month` is 1–12.
fn review(year: i32, plays: &[(HistoryEntry, u32)]) -> YearReview {
    let mut months = [0i64; 12];
    let mut listened_ms = 0u64;
    // Per track: the newest copy of its metadata (a retagged track shows as it
    // is now) and how often it was played.
    let mut by_track: HashMap<&str, (&HistoryTrack, i64)> = HashMap::new();
    let mut by_album: HashMap<String, AlbumPlays> = HashMap::new();
    for (entry, month) in plays {
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
                track.album.to_lowercase(),
                track.artist.to_lowercase()
            ),
        };
        let album = by_album.entry(key).or_insert_with(|| AlbumPlays {
            album_id: track.album_id.clone(),
            name: track.album.clone(),
            artist: track.artist.clone(),
            image_item_id: None,
            image_tag: None,
            image_blur_hash: None,
            plays: 0,
        });
        album.plays += 1;
        // The album's own picture where the track has one of it; a track
        // without art leaves what an earlier one found.
        if track.image_item_id.is_some() {
            album.image_item_id = track.image_item_id.clone();
            album.image_tag = track.image_tag.clone();
            album.image_blur_hash = track.image_blur_hash.clone();
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

    let mut top_albums: Vec<AlbumPlays> = by_album.into_values().collect();
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

/// Forget the history of the account signed in now. Other accounts keep
/// theirs.
pub fn clear(store: &Store) -> AppResult<usize> {
    let Some((server, user)) = owner(store) else {
        return Ok(0);
    };
    store.with_conn(|conn| {
        conn.execute(
            "DELETE FROM play_history WHERE server_url = ?1 AND user_id = ?2",
            params![server, user],
        )
    })
}

/// Forget one listen.
pub fn delete(store: &Store, id: i64) -> AppResult<bool> {
    let Some((server, user)) = owner(store) else {
        return Ok(false);
    };
    let removed = store.with_conn(|conn| {
        conn.execute(
            "DELETE FROM play_history WHERE id = ?1 AND server_url = ?2 AND user_id = ?3",
            params![id, server, user],
        )
    })?;
    Ok(removed > 0)
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

    /// Noon on a day of 2026, local time, as Unix ms — far enough from
    /// midnight that no time zone moves it into another month.
    fn day(month: u32, day: u32) -> i64 {
        let days_before = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let since_epoch = 20_454 + days_before[(month - 1) as usize] + i64::from(day - 1);
        (since_epoch * 86_400 + 12 * 3_600) * 1_000
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
