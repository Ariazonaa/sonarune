pub mod types;

use crate::error::{AppError, AppResult};
use serde::de::DeserializeOwned;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use types::*;

pub const CLIENT_NAME: &str = "Jellysic";
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The download name from a Content-Disposition header.
///
/// Prefers the RFC 6266 / 5987 `filename*=` form: ASP.NET Core (Jellyfin)
/// replaces every non-ASCII character of the plain `filename=` with `_` and
/// puts the real name only in `filename*=UTF-8''<percent-encoded>`. Falls back
/// to `filename=` when the extended value is absent or cannot be decoded.
/// The result is untrusted: callers run it through the filename sanitizer.
fn content_disposition_filename(header: &str) -> Option<String> {
    let params = content_disposition_params(header);
    let extended = params
        .iter()
        .filter(|(name, _)| name == "filename*")
        .find_map(|(_, value)| decode_ext_value(value))
        .filter(|name| !name.is_empty());
    extended.or_else(|| {
        params
            .into_iter()
            .find(|(name, _)| name == "filename")
            .map(|(_, value)| value)
            .filter(|name| !name.is_empty())
    })
}

/// `name=value` parameters of a Content-Disposition header, names lowercased.
/// Quote-aware: a `;` inside a quoted value does not end it, and `\` escapes
/// the next character there. The disposition type has no `=` and is skipped.
fn content_disposition_params(header: &str) -> Vec<(String, String)> {
    let mut params = Vec::new();
    let mut chars = header.chars().peekable();
    loop {
        let mut name = String::new();
        while let Some(c) = chars.next_if(|&c| c != '=' && c != ';') {
            name.push(c);
        }
        if chars.next_if_eq(&'=').is_some() {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            let mut value = String::new();
            if chars.next_if_eq(&'"').is_some() {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => value.extend(chars.next()),
                        '"' => break,
                        c => value.push(c),
                    }
                }
                // Anything between the closing quote and the next `;` is junk.
                while chars.next_if(|&c| c != ';').is_some() {}
            } else {
                while let Some(c) = chars.next_if(|&c| c != ';') {
                    value.push(c);
                }
                value = value.trim().to_string();
            }
            params.push((name.trim().to_ascii_lowercase(), value));
        }
        // Consume the `;` separating the next parameter, or stop at the end.
        if chars.next().is_none() {
            break;
        }
    }
    params
}

/// Decode an RFC 5987 `ext-value` (`charset'language'percent-encoded`). The
/// language tag is ignored; UTF-8 and ISO-8859-1 (the two charsets the RFC
/// requires) are supported. `None` for anything malformed.
fn decode_ext_value(value: &str) -> Option<String> {
    // Not allowed by the grammar, but tolerate a quoted ext-value.
    let value = value.trim().trim_matches('"');
    let mut parts = value.splitn(3, '\'');
    let (charset, _language, encoded) = (parts.next()?, parts.next()?, parts.next()?);
    let bytes = percent_decode(encoded)?;
    if charset.eq_ignore_ascii_case("utf-8") {
        String::from_utf8(bytes).ok()
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        Some(bytes.into_iter().map(char::from).collect())
    } else {
        None
    }
}

/// `%XX` → byte; any other byte passes through. `None` on a broken escape.
fn percent_decode(encoded: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(encoded.len());
    let mut bytes = encoded.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = char::from(bytes.next()?).to_digit(16)?;
            let lo = char::from(bytes.next()?).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        } else {
            out.push(b);
        }
    }
    Some(out)
}

/// Most ids one request carries in its query string. Kestrel rejects a
/// request line over 8 KB (414), and a GUID costs ~35 bytes once its `%2C`
/// separator is encoded; 100 stays far below that next to the other params.
pub const MAX_IDS_PER_REQUEST: usize = 100;

/// `ids` without repeats, first occurrence first — a repeated id would only
/// cost request space, the by-id results are keyed by id anyway.
fn unique_ids(ids: &[String]) -> Vec<&str> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter()
        .map(String::as_str)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Most of a non-2xx body read for the error message — a proxy's error page
/// can be arbitrarily large and is never worth buffering whole.
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;
/// Error messages end up in the UI banner and the log; keep them short.
const MAX_ERROR_MESSAGE_CHARS: usize = 300;

/// A bounded, banner-sized message from a failed response's body.
async fn error_message(mut response: reqwest::Response) -> String {
    let mut body = Vec::new();
    while body.len() < MAX_ERROR_BODY_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = MAX_ERROR_BODY_BYTES - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            // The message is best effort; a broken body just ends it.
            Ok(None) | Err(_) => break,
        }
    }
    shorten_error_message(&String::from_utf8_lossy(&body))
}

/// Collapse whitespace (HTML error pages are mostly line breaks) and cut to
/// `MAX_ERROR_MESSAGE_CHARS` on a character boundary.
fn shorten_error_message(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.char_indices().nth(MAX_ERROR_MESSAGE_CHARS) {
        Some((cut, _)) => format!("{}…", collapsed[..cut].trim_end()),
        None => collapsed,
    }
}

/// Reject malformed selections before contacting Jellyfin.
fn validate_playlist_entry_ids(entry_ids: &[String]) -> AppResult<()> {
    if entry_ids.is_empty() {
        return Err(AppError::Other("no playlist entries selected".into()));
    }

    let mut requested = HashSet::with_capacity(entry_ids.len());
    for entry_id in entry_ids {
        if entry_id.trim().is_empty() {
            return Err(AppError::Other(
                "playlist entry id must not be empty".into(),
            ));
        }
        if !requested.insert(entry_id.as_str()) {
            return Err(AppError::Other(format!(
                "duplicate playlist entry id: {entry_id}"
            )));
        }
    }

    Ok(())
}

/// Resolve a selection of playlist entry ids to the item ids Jellyfin's add
/// endpoint expects. The caller's selection order is deliberately ignored:
/// copied tracks keep their order in the freshly fetched source playlist.
fn resolve_playlist_item_ids<'a>(
    source_entries: impl IntoIterator<Item = (&'a str, &'a str)>,
    entry_ids: &[String],
) -> AppResult<Vec<String>> {
    validate_playlist_entry_ids(entry_ids)?;
    let requested = entry_ids.iter().map(String::as_str).collect::<HashSet<_>>();

    let mut matched = HashSet::with_capacity(requested.len());
    let mut item_ids = Vec::with_capacity(requested.len());
    for (entry_id, item_id) in source_entries {
        if requested.contains(entry_id) && matched.insert(entry_id) {
            item_ids.push(item_id.to_string());
        }
    }

    if matched.len() != requested.len() {
        let unknown = entry_ids
            .iter()
            .filter(|entry_id| !matched.contains(entry_id.as_str()))
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::Other(format!(
            "playlist entries not found in source playlist: {unknown}"
        )));
    }

    Ok(item_ids)
}

const SMART_SCAN_BATCH: u32 = 500;
/// Hard ceiling on a paginated scan. Also bounds a genuinely huge library, not
/// just a hostile server: the whole result is held in memory before it is
/// reduced to album ids.
const SMART_SCAN_MAX_ITEMS: usize = 100_000;

/// How long a later page of a smart view may reuse the scan the first page did.
/// The first page always rescans, so a new query (filter change, library
/// refresh) never sees stale results; this only bounds how old the tail of a
/// view can get while the user pages through it.
const SMART_SCAN_CACHE_TTL: Duration = Duration::from_secs(120);

/// Result ids of the last full scan for one kind of paged view, keyed by the
/// query that produced them. Without it, every "load more" on a view that is
/// evaluated locally rescanned the whole library. Ids only — the page itself
/// is fetched by id, so the matches' items are not held in memory between
/// pages.
#[derive(Debug)]
struct ScanCache<K> {
    key: K,
    scanned_at: Instant,
    ids: Arc<Vec<String>>,
}

impl<K: PartialEq> ScanCache<K> {
    fn ids_for(&self, key: &K, now: Instant, ttl: Duration) -> Option<Arc<Vec<String>>> {
        (self.key == *key && now.saturating_duration_since(self.scanned_at) <= ttl)
            .then(|| self.ids.clone())
    }
}

/// Page size of the album grid's scans (played tracks, matching album ids).
/// Both requests are ids plus user data only, so a large page stays cheap.
const ALBUM_SCAN_BATCH: u32 = 1000;

/// How long later pages of a locally evaluated album grid reuse its scan. As
/// with smart views, the first page always rescans.
const ALBUM_SCAN_CACHE_TTL: Duration = Duration::from_secs(60);

/// Sort keys of the album grid (`get_albums`'s `sort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlbumSort {
    Name,
    DateAdded,
    Year,
    Random,
    PlayCount,
    DatePlayed,
}

impl AlbumSort {
    fn parse(sort: &str) -> Self {
        match sort {
            "dateAdded" => Self::DateAdded,
            "year" => Self::Year,
            "random" => Self::Random,
            "playCount" => Self::PlayCount,
            "datePlayed" => Self::DatePlayed,
            _ => Self::Name,
        }
    }

    /// Sorted by play data, which the server does not have for albums:
    /// playback updates PlayCount/LastPlayedDate on the tracks only, so an
    /// album-level `sortBy=PlayCount|DatePlayed` has nothing to sort by.
    fn is_play_based(self) -> bool {
        matches!(self, Self::PlayCount | Self::DatePlayed)
    }

    /// Jellyfin's `(sortBy, sortOrder)` for this sort, with one order per key:
    /// 10.11 applies the first order to every key that has none of its own,
    /// so a lone `Descending` would sort the SortName tie-break Z→A. The
    /// play-based sorts are ordered locally on top of name order.
    fn server_order(self) -> (&'static str, &'static str) {
        match self {
            Self::DateAdded => ("DateCreated,SortName", "Descending,Ascending"),
            Self::Year => ("ProductionYear,SortName", "Descending,Ascending"),
            Self::Random => ("Random", "Ascending"),
            Self::Name | Self::PlayCount | Self::DatePlayed => ("SortName", "Ascending"),
        }
    }
}

fn parse_played_filter(played: &str) -> PlayedFilter {
    match played {
        "played" => PlayedFilter::Played,
        "unplayed" => PlayedFilter::Unplayed,
        _ => PlayedFilter::All,
    }
}

/// Everything that decides which albums a locally evaluated grid shows and
/// in which order — the scan cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AlbumGridQuery {
    user_id: String,
    genre_id: Option<String>,
    sort: AlbumSort,
    favorites_only: bool,
    played: PlayedFilter,
}

impl AlbumGridQuery {
    /// Whether the server can filter and order this grid itself, and so page
    /// it. Played state and play data live on tracks, not albums (an album's
    /// `IsPlayed`/`IsUnplayed` filter reads the album's own, never-written
    /// user data): those need the local scan.
    fn is_server_side(&self) -> bool {
        !self.sort.is_play_based() && self.played == PlayedFilter::All
    }
}

/// Play data of one album, folded from its played tracks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AlbumPlayStats {
    /// Sum of the tracks' play counts.
    play_count: i64,
    /// Latest `LastPlayedDate` of any track.
    last_played: Option<String>,
}

/// Played tracks folded into per-album play stats, one scan page at a time.
#[derive(Debug, Default)]
struct AlbumPlayAggregate {
    /// Guards the sums against a track that two scan pages both returned.
    seen_tracks: HashSet<String>,
    albums: HashMap<String, AlbumPlayStats>,
}

impl AlbumPlayAggregate {
    /// Add tracks the server reported as played (`filters=IsPlayed`). Such a
    /// track marks its album played even at play count 0 (marked played by
    /// hand); tracks without an album are ignored.
    fn add(&mut self, items: impl IntoIterator<Item = BaseItem>) {
        for item in items {
            let Some(album_id) = item.album_id.filter(|id| !id.is_empty()) else {
                continue;
            };
            if !item.id.is_empty() && !self.seen_tracks.insert(item.id) {
                continue;
            }
            let stats = self.albums.entry(album_id).or_default();
            let Some(data) = item.user_data else {
                continue;
            };
            stats.play_count += i64::from(data.play_count.max(0));
            // ISO-8601 UTC timestamps in one format compare as text.
            if let Some(date) = data.last_played_date.filter(|date| !date.is_empty()) {
                if stats
                    .last_played
                    .as_deref()
                    .is_none_or(|latest| date.as_str() > latest)
                {
                    stats.last_played = Some(date);
                }
            }
        }
    }
}

/// The locally evaluated part of the album grid. `album_ids` are the albums
/// that pass the server-side filters (genre, favorites), in server order;
/// `stats` holds the albums with at least one played track. Applies the
/// played filter, then orders by play data for a play-based sort (most first).
fn select_grid_albums(
    album_ids: Vec<String>,
    stats: &HashMap<String, AlbumPlayStats>,
    played: PlayedFilter,
    sort: AlbumSort,
) -> Vec<String> {
    let ids = match played {
        PlayedFilter::All => album_ids,
        PlayedFilter::Played => album_ids
            .into_iter()
            .filter(|id| stats.contains_key(id))
            .collect(),
        PlayedFilter::Unplayed => album_ids
            .into_iter()
            .filter(|id| !stats.contains_key(id))
            .collect(),
    };
    match sort {
        AlbumSort::PlayCount => rank_album_ids(ids, true, |id| {
            stats
                .get(id)
                .map(|s| s.play_count)
                .filter(|&count| count > 0)
        }),
        AlbumSort::DatePlayed => rank_album_ids(ids, true, |id| {
            stats.get(id).and_then(|s| s.last_played.as_deref())
        }),
        AlbumSort::Name | AlbumSort::DateAdded | AlbumSort::Year | AlbumSort::Random => ids,
    }
}

/// Order `ids` by `key` in the given direction. Stable, so ties keep their
/// incoming (name) order; ids without a key — albums never played — follow
/// the ranked ones, still in incoming order, whatever the direction.
fn rank_album_ids<K: Ord>(
    ids: Vec<String>,
    descending: bool,
    key: impl Fn(&str) -> Option<K>,
) -> Vec<String> {
    let mut ranked = Vec::new();
    let mut unranked = Vec::new();
    for id in ids {
        match key(&id) {
            Some(k) => ranked.push((k, id)),
            None => unranked.push(id),
        }
    }
    ranked.sort_by(|a, b| {
        let order = a.0.cmp(&b.0);
        if descending {
            order.reverse()
        } else {
            order
        }
    });
    ranked
        .into_iter()
        .map(|(_, id)| id)
        .chain(unranked)
        .collect()
}

/// The `limit` ids from `start_index` on; empty past the end.
fn page_of_ids(ids: &[String], start_index: u32, limit: u32) -> Vec<String> {
    ids.iter()
        .skip(start_index as usize)
        .take(limit as usize)
        .cloned()
        .collect()
}

/// The album grid's `/Items` query without paging and payload flags: the
/// user, albums, the server-side filters (genre, favorites) and the server
/// order of `sort`. Never an album-level played filter — see
/// [`AlbumGridQuery::is_server_side`].
fn album_grid_params(query: &AlbumGridQuery, sort: AlbumSort) -> Vec<(&'static str, String)> {
    let (sort_by, sort_order) = sort.server_order();
    let mut params = vec![
        ("userId", query.user_id.clone()),
        ("includeItemTypes", "MusicAlbum".to_string()),
        ("recursive", "true".to_string()),
        ("sortBy", sort_by.to_string()),
        ("sortOrder", sort_order.to_string()),
    ];
    if let Some(genre_id) = &query.genre_id {
        params.push(("genreIds", genre_id.clone()));
    }
    if query.favorites_only {
        params.push(("filters", "IsFavorite".to_string()));
    }
    params
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SmartQueryPlan {
    filters: Option<String>,
    genre_ids: Option<String>,
    years: Option<String>,
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn valid_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Ok(year) = value[0..4].parse::<i32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u32>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<u32>() else {
        return false;
    };
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=max_day).contains(&day)
}

pub(crate) fn validate_smart_filter(filter: &SmartFilter) -> AppResult<()> {
    for (label, year) in [("yearFrom", filter.year_from), ("yearTo", filter.year_to)] {
        if let Some(year) = year {
            if !(1..=9999).contains(&year) {
                return Err(AppError::Other(format!(
                    "{label} must be between 1 and 9999"
                )));
            }
        }
    }
    if let (Some(from), Some(to)) = (filter.year_from, filter.year_to) {
        if from > to {
            return Err(AppError::Other(
                "yearFrom must not be greater than yearTo".into(),
            ));
        }
        if to - from > 300 {
            return Err(AppError::Other(
                "year range must not span more than 300 years".into(),
            ));
        }
    }

    let mut genre_ids = HashSet::with_capacity(filter.genre_ids.len());
    for id in &filter.genre_ids {
        if id.trim().is_empty() {
            return Err(AppError::Other("genre id must not be empty".into()));
        }
        if !genre_ids.insert(id.as_str()) {
            return Err(AppError::Other(format!("duplicate genre id: {id}")));
        }
    }

    if filter.min_play_count.is_some_and(|count| count < 0) {
        return Err(AppError::Other("minPlayCount must not be negative".into()));
    }
    if let Some(date) = filter.added_since.as_deref() {
        if !valid_iso_date(date) {
            return Err(AppError::Other(
                "addedSince must be a valid YYYY-MM-DD date".into(),
            ));
        }
    }
    Ok(())
}

fn smart_query_plan(filter: &SmartFilter) -> AppResult<SmartQueryPlan> {
    validate_smart_filter(filter)?;
    let mut filters = Vec::new();
    if filter.favorite_only {
        filters.push("IsFavorite");
    }
    match filter.played {
        PlayedFilter::All => {}
        PlayedFilter::Played => filters.push("IsPlayed"),
        PlayedFilter::Unplayed => filters.push("IsUnplayed"),
    }
    let years = match (filter.year_from, filter.year_to) {
        (Some(from), Some(to)) => Some(
            (from..=to)
                .map(|year| year.to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
        _ => None,
    };
    Ok(SmartQueryPlan {
        filters: (!filters.is_empty()).then(|| filters.join(",")),
        genre_ids: (!filter.genre_ids.is_empty()).then(|| filter.genre_ids.join(",")),
        years,
    })
}

/// Whether the server evaluates `filter` completely, so a page can be fetched
/// with the server's own `startIndex`/`limit`. `minPlayCount`, `addedSince`
/// and an open-ended year range have no server-side query parameter; they are
/// checked locally and need a scan to know which tracks match.
fn smart_filter_is_server_side(filter: &SmartFilter) -> bool {
    filter.min_play_count.unwrap_or(0) <= 0
        && filter.added_since.is_none()
        && filter.year_from.is_some() == filter.year_to.is_some()
}

/// Whether a scan batch already reached tracks added before `since`. The scan
/// is sorted by DateCreated, newest first, so everything after such a track is
/// older still and cannot match — an "added this month" view stops there
/// instead of walking the whole library.
fn reached_added_cutoff(items: &[BaseItem], since: &str) -> bool {
    items.iter().any(|item| {
        item.date_created
            .as_deref()
            .and_then(|date| date.get(0..10))
            .is_some_and(|day| day < since)
    })
}

fn smart_item_matches(item: &BaseItem, filter: &SmartFilter) -> bool {
    if filter
        .year_from
        .is_some_and(|from| item.production_year.is_none_or(|year| year < from))
    {
        return false;
    }
    if filter
        .year_to
        .is_some_and(|to| item.production_year.is_none_or(|year| year > to))
    {
        return false;
    }
    if !filter.genre_ids.is_empty()
        && !item
            .genre_items
            .iter()
            .any(|genre| filter.genre_ids.iter().any(|id| id == &genre.id))
    {
        return false;
    }
    if filter.min_play_count.is_some_and(|minimum| {
        item.user_data
            .as_ref()
            .map(|data| data.play_count)
            .unwrap_or(0)
            < minimum
    }) {
        return false;
    }
    if let Some(since) = filter.added_since.as_deref() {
        let Some(created) = item
            .date_created
            .as_deref()
            .and_then(|date| date.get(0..10))
        else {
            return false;
        };
        if created < since {
            return false;
        }
    }
    true
}

fn unique_album_ids(items: impl IntoIterator<Item = BaseItem>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter_map(|item| item.album_id)
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

fn flatten_album_tracks<T>(albums: impl IntoIterator<Item = Vec<T>>) -> Vec<T> {
    albums.into_iter().flatten().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionValidity {
    Valid,
    /// Token rejected — re-authentication needed.
    Invalid,
    /// Server not reachable or unhealthy; token may well still be good.
    Unreachable,
}

/// Fingerprint of the music library used to detect server-side changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySignature {
    /// `DateCreated` of the most recently added track.
    pub newest: String,
    /// Total number of audio tracks.
    pub count: i64,
}

/// Thin typed Jellyfin client. All server traffic goes through here (reqwest),
/// never through the WebView — home servers with plain HTTP or self-signed
/// certificates must work.
pub struct JellyfinClient {
    http: reqwest::Client,
    base_url: String,
    device_id: String,
    /// Confirmed SHA-256 fingerprints this client's TLS backend also accepts on
    /// top of the OS trust store.
    trusted: Vec<String>,
    /// Fingerprint the most recent handshake presented (set by the TLS verifier).
    observed: crate::tls::ObservedFingerprint,
    token: Option<String>,
    user_id: Option<String>,
    /// Last full smart-view scan, reused by that view's later pages. Per client,
    /// so it never outlives the session (server and user) it was scanned for.
    smart_scan_cache: Mutex<Option<ScanCache<SmartFilter>>>,
    /// Last locally evaluated album-grid scan (play-based sort or played
    /// filter), reused by that grid's later pages. Per client, like the above.
    album_scan_cache: Mutex<Option<ScanCache<AlbumGridQuery>>>,
    /// The last 401 follow-up probe: when it started and what it found. The
    /// async lock makes the probe single-flight (see [`Self::validate_after`]).
    token_probe: tokio::sync::Mutex<Option<(Instant, SessionValidity)>>,
}

/// Ceiling for a cover image. Jellyfin's largest rendition is far below this;
/// the point is that a hostile or broken server cannot make the client buffer
/// an unbounded body in memory (the bytes are held three times over: response
/// buffer, cache write, and WebView response).
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Read a response body with a hard ceiling, streaming so the limit bites
/// before the memory is committed rather than after. A declared
/// `Content-Length` over the limit is rejected without reading at all.
async fn read_capped(response: reqwest::Response, limit: u64, what: &str) -> AppResult<Vec<u8>> {
    if response.content_length().is_some_and(|len| len > limit) {
        return Err(AppError::Other(format!("{what} exceeds the size limit")));
    }
    let mut response = response;
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if out.len() as u64 + chunk.len() as u64 > limit {
            return Err(AppError::Other(format!("{what} exceeds the size limit")));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

impl JellyfinClient {
    pub fn new(base_url: &str, device_id: &str, trusted: Vec<String>) -> AppResult<Self> {
        let base = base_url.trim_end_matches('/').to_string();
        let (builder, observed) = crate::tls::apply(
            reqwest::Client::builder()
                // Stay on the configured server: a redirect off-origin would
                // send requests (and, on a same-scheme host, our credentials)
                // somewhere the user never configured, and an https -> http
                // redirect would silently drop TLS.
                .redirect(crate::player::source::same_origin_redirects(base.clone()))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30)),
            &trusted,
        )?;
        let http = builder.build()?;
        Ok(Self {
            http,
            base_url: base,
            device_id: device_id.to_string(),
            trusted,
            observed,
            token: None,
            user_id: None,
            smart_scan_cache: Mutex::new(None),
            album_scan_cache: Mutex::new(None),
            token_probe: tokio::sync::Mutex::new(None),
        })
    }

    pub fn with_session(mut self, token: String, user_id: String) -> Self {
        self.token = Some(token);
        self.user_id = Some(user_id);
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Fingerprints this client trusts on top of the OS trust store — snapshot
    /// used to build the player's streaming client the same way.
    pub fn trusted_fingerprints(&self) -> &[String] {
        &self.trusted
    }

    /// The fingerprint the last handshake presented when its certificate is not
    /// publicly valid — pinned or not. `None` for a publicly trusted certificate
    /// (nothing to pin) or before any handshake.
    pub fn presented_fingerprint(&self) -> Option<String> {
        self.observed.lock().unwrap().clone()
    }

    /// The fingerprint the last handshake presented, but only when it is *not*
    /// already trusted — i.e. the certificate that a failed connection was
    /// rejected for, offered up for the user to confirm.
    pub fn untrusted_fingerprint(&self) -> Option<String> {
        let observed = self.observed.lock().unwrap().clone()?;
        if self
            .trusted
            .iter()
            .any(|t| crate::tls::matches(t, &observed))
        {
            None
        } else {
            Some(observed)
        }
    }

    pub fn user_id(&self) -> AppResult<&str> {
        self.user_id.as_deref().ok_or(AppError::NotConnected)
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// `Authorization: MediaBrowser ...` — the only scheme that survives the
    /// legacy-auth removal planned for Jellyfin 12/13.
    pub fn auth_header(&self) -> String {
        let mut header = format!(
            "MediaBrowser Client=\"{CLIENT_NAME}\", Device=\"Windows\", DeviceId=\"{}\", Version=\"{CLIENT_VERSION}\"",
            self.device_id
        );
        if let Some(token) = &self.token {
            header.push_str(&format!(", Token=\"{token}\""));
        }
        header
    }

    pub async fn authenticate(&mut self, username: &str, password: &str) -> AppResult<UserDto> {
        let url = format!("{}/Users/AuthenticateByName", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "Username": username, "Pw": password }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let message = match status.as_u16() {
                401 => "wrong username or password".to_string(),
                _ => error_message(response).await,
            };
            return Err(AppError::Server {
                status: status.as_u16(),
                message,
            });
        }
        let auth: AuthenticationResult = response.json().await?;
        self.token = Some(auth.access_token);
        self.user_id = Some(auth.user.id.clone());
        Ok(auth.user)
    }

    /// Cheap token validation used on session restore. Distinguishes a dead
    /// token from an unreachable server: offline must not log the user out.
    pub async fn validate(&self) -> SessionValidity {
        let url = format!("{}/Users/Me", self.base_url);
        match self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => SessionValidity::Valid,
            Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => {
                SessionValidity::Invalid
            }
            // 5xx, proxies mid-restart, network errors: server trouble, not
            // an auth problem.
            Ok(_) => SessionValidity::Unreachable,
            Err(_) => SessionValidity::Unreachable,
        }
    }

    /// [`Self::validate`] for a call that got a 401 at `unauthorized_at`,
    /// single-flight so a burst of 401s on this session costs one probe.
    ///
    /// Callers queue on the probe. A finished probe answers for every 401 that
    /// arrived before it *started*: it saw the token at least as late as that
    /// 401 did (a rejected token never becomes valid again, so "valid" rules
    /// out an expired token). A 401 that is newer than the last probe gets a
    /// fresh one.
    pub async fn validate_after(&self, unauthorized_at: Instant) -> SessionValidity {
        let mut last = self.token_probe.lock().await;
        if let Some((started, validity)) = *last {
            if started >= unauthorized_at {
                return validity;
            }
        }
        let started = Instant::now();
        let validity = self.validate().await;
        *last = Some((started, validity));
        validity
    }

    /// The current user, including policy — used to refresh the delete
    /// permission on session restore (the token path skips authenticate()).
    pub async fn me(&self) -> AppResult<UserDto> {
        self.get_json("/Users/Me", &[]).await
    }

    /// The complete item as raw JSON, for a faithful metadata round-trip. Uses
    /// `/Users/{userId}/Items/{itemId}` — the singular `/Items/{itemId}` path
    /// returns 405, and this is the only fetch that populates every field.
    pub async fn get_item_raw(&self, item_id: &str) -> AppResult<serde_json::Value> {
        // `userId` belongs in the query string: the `/Users/{id}/Items/...`
        // path form is the legacy route Jellyfin is retiring.
        let user_id = self.user_id()?.to_string();
        self.get_json(&format!("/Items/{item_id}"), &[("userId", user_id)])
            .await
    }

    /// POST the whole (edited) item back. A 403 here means the user isn't an admin.
    pub async fn update_item(&self, item_id: &str, body: serde_json::Value) -> AppResult<()> {
        self.post_no_content(&format!("/Items/{item_id}"), body)
            .await
    }

    /// Version only; diagnostics deliberately do not expose the server name,
    /// id, address, or any other identifying system-info fields.
    pub async fn server_version(&self) -> AppResult<String> {
        let info: PublicSystemInfo = self.get_json("/System/Info/Public", &[]).await?;
        Ok(info.version)
    }

    async fn post_no_content(&self, path: &str, body: serde_json::Value) -> AppResult<()> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        Ok(())
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> AppResult<T> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .query(query)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        Ok(response.json().await?)
    }

    /// One page of the album grid.
    ///
    /// The server filters, orders and pages the grid itself unless it needs
    /// play data (a play-based sort or the played filter, see
    /// [`AlbumGridQuery::is_server_side`]). Then only a scan knows the result:
    /// the first page scans the played tracks and the albums passing the
    /// server-side filters and evaluates the rest locally; later pages fetch
    /// their slice by id from that scan's ids.
    pub async fn albums(
        &self,
        start_index: u32,
        limit: u32,
        genre_id: Option<&str>,
        sort: &str,
        favorites_only: bool,
        played: &str,
    ) -> AppResult<Page<AlbumDto>> {
        let query = AlbumGridQuery {
            user_id: self.user_id()?.to_string(),
            genre_id: genre_id.map(str::to_string),
            sort: AlbumSort::parse(sort),
            favorites_only,
            played: parse_played_filter(played),
        };
        if !query.is_server_side() {
            let ids = self.album_grid_ids(&query, start_index).await?;
            return Ok(Page {
                items: self
                    .albums_by_ids(&page_of_ids(&ids, start_index, limit))
                    .await?,
                total: ids.len() as i64,
            });
        }

        let mut params = album_grid_params(&query, query.sort);
        params.extend([
            ("startIndex", start_index.to_string()),
            ("limit", limit.to_string()),
            ("enableImages", "true".into()),
            ("imageTypeLimit", "1".into()),
            ("enableUserData", "true".into()),
        ]);
        let response: ItemsResponse = self.get_json("/Items", &params).await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(AlbumDto::from_item)
                .collect(),
        })
    }

    /// Every album id of a locally evaluated grid, in grid order. The first
    /// page always rescans, so a changed query or new plays are never served
    /// from an old scan; later pages reuse it while it is fresh.
    async fn album_grid_ids(
        &self,
        query: &AlbumGridQuery,
        start_index: u32,
    ) -> AppResult<Arc<Vec<String>>> {
        if start_index > 0 {
            let cached = self
                .album_scan_cache
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|cache| cache.ids_for(query, Instant::now(), ALBUM_SCAN_CACHE_TTL));
            if let Some(ids) = cached {
                return Ok(ids);
            }
        }

        let (stats, album_ids) = tokio::try_join!(
            self.album_play_stats(&query.user_id),
            self.album_grid_scan(query),
        )?;
        let mut ids = select_grid_albums(album_ids, &stats, query.played, query.sort);
        if query.sort == AlbumSort::Random {
            crate::commands_discover::shuffle_items(&mut ids);
        }
        let ids = Arc::new(ids);
        *self.album_scan_cache.lock().unwrap() = Some(ScanCache {
            key: query.clone(),
            scanned_at: Instant::now(),
            ids: ids.clone(),
        });
        Ok(ids)
    }

    /// Ids of every album passing the grid's server-side filters, in the
    /// server order the grid builds on. A random grid is scanned in name order
    /// and shuffled afterwards: `sortBy=Random` reshuffles on every page
    /// request, so its pages would overlap and miss albums.
    async fn album_grid_scan(&self, query: &AlbumGridQuery) -> AppResult<Vec<String>> {
        let sort = match query.sort {
            AlbumSort::Random => AlbumSort::Name,
            sort => sort,
        };
        let mut params = album_grid_params(query, sort);
        params.extend([
            ("enableImages", "false".into()),
            ("enableUserData", "false".into()),
        ]);
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        self.scan_items(&params, |items| {
            ids.extend(
                items
                    .into_iter()
                    .map(|item| item.id)
                    .filter(|id| seen.insert(id.clone())),
            );
        })
        .await?;
        Ok(ids)
    }

    /// Per-album play stats of `user_id`, folded from a scan of every played
    /// track — the only items playback writes play data to.
    async fn album_play_stats(&self, user_id: &str) -> AppResult<HashMap<String, AlbumPlayStats>> {
        let params = [
            ("userId", user_id.to_string()),
            ("includeItemTypes", "Audio".to_string()),
            ("recursive", "true".to_string()),
            ("filters", "IsPlayed".to_string()),
            ("sortBy", "SortName".to_string()),
            ("sortOrder", "Ascending".to_string()),
            ("enableImages", "false".to_string()),
            ("enableUserData", "true".to_string()),
        ];
        let mut aggregate = AlbumPlayAggregate::default();
        self.scan_items(&params, |items| aggregate.add(items))
            .await?;
        Ok(aggregate.albums)
    }

    /// Walk every page of an `/Items` query, [`ALBUM_SCAN_BATCH`] items at a
    /// time, handing each page to `take` so a caller keeps only what it needs.
    async fn scan_items(
        &self,
        query: &[(&'static str, String)],
        mut take: impl FnMut(Vec<BaseItem>),
    ) -> AppResult<()> {
        let mut offset = 0u32;
        let mut scanned = 0usize;
        loop {
            let mut page = query.to_vec();
            page.push(("startIndex", offset.to_string()));
            page.push(("limit", ALBUM_SCAN_BATCH.to_string()));
            let response: ItemsResponse = self.get_json("/Items", &page).await?;
            let received = response.items.len() as u32;
            let total = response.total_record_count;
            take(response.items);
            offset = offset.saturating_add(received);
            scanned = scanned.saturating_add(received as usize);
            // Same bounds as the smart-view scan: `total_record_count` is a
            // server-supplied number and must not be the only stop condition.
            if received == 0
                || received < ALBUM_SCAN_BATCH
                || scanned >= SMART_SCAN_MAX_ITEMS
                || offset == u32::MAX
                || i64::from(offset) >= total
            {
                if scanned >= SMART_SCAN_MAX_ITEMS {
                    tracing::warn!("library scan stopped at {scanned} items; results are partial");
                }
                break;
            }
        }
        Ok(())
    }

    pub async fn album(&self, album_id: &str) -> AppResult<AlbumDto> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("ids", album_id.to_string()),
                    ("enableImages", "true".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        response
            .items
            .into_iter()
            .next()
            .map(AlbumDto::from_item)
            .ok_or_else(|| AppError::Other(format!("album {album_id} not found")))
    }

    pub async fn album_tracks(&self, album_id: &str) -> AppResult<Vec<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("parentId", album_id.to_string()),
                    ("includeItemTypes", "Audio".into()),
                    ("sortBy", "ParentIndexNumber,IndexNumber,SortName".into()),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(TrackDto::from_item)
            .collect())
    }

    // --- Playback reporting (drives play counts, "recently played", and
    // server-side scrobble plugins). Ticks are .NET ticks: 100 ns. ---

    pub async fn report_start(&self, item_id: &str, play_session_id: &str) -> AppResult<()> {
        self.post_no_content(
            "/Sessions/Playing",
            serde_json::json!({
                "ItemId": item_id,
                "PlaySessionId": play_session_id,
                "PositionTicks": 0,
                "CanSeek": true,
                "IsPaused": false,
                "PlayMethod": "DirectPlay",
                "RepeatMode": "RepeatNone",
            }),
        )
        .await
    }

    pub async fn report_progress(
        &self,
        item_id: &str,
        play_session_id: &str,
        position_ms: u64,
        is_paused: bool,
    ) -> AppResult<()> {
        self.post_no_content(
            "/Sessions/Playing/Progress",
            serde_json::json!({
                "ItemId": item_id,
                "PlaySessionId": play_session_id,
                "PositionTicks": position_ms * 10_000,
                "CanSeek": true,
                "IsPaused": is_paused,
                "PlayMethod": "DirectPlay",
                "RepeatMode": "RepeatNone",
            }),
        )
        .await
    }

    pub async fn report_stopped(
        &self,
        item_id: &str,
        play_session_id: &str,
        position_ms: u64,
    ) -> AppResult<()> {
        self.post_no_content(
            "/Sessions/Playing/Stopped",
            serde_json::json!({
                "ItemId": item_id,
                "PlaySessionId": play_session_id,
                "PositionTicks": position_ms * 10_000,
            }),
        )
        .await
    }

    // --- Quick Connect (login without typing a password) ---

    pub async fn quick_connect_initiate(&self) -> AppResult<QuickConnectResult> {
        let url = format!("{}/QuickConnect/Initiate", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let message = if status.as_u16() == 401 {
                "Quick Connect is disabled on this server".to_string()
            } else {
                error_message(response).await
            };
            return Err(AppError::Server {
                status: status.as_u16(),
                message,
            });
        }
        Ok(response.json().await?)
    }

    /// Poll until the user approved the code on another device.
    pub async fn quick_connect_state(&self, secret: &str) -> AppResult<QuickConnectResult> {
        self.get_json("/QuickConnect/Connect", &[("secret", secret.to_string())])
            .await
    }

    pub async fn authenticate_with_quick_connect(&mut self, secret: &str) -> AppResult<UserDto> {
        let url = format!("{}/Users/AuthenticateWithQuickConnect", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "Secret": secret }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        let auth: AuthenticationResult = response.json().await?;
        self.token = Some(auth.access_token);
        self.user_id = Some(auth.user.id.clone());
        Ok(auth.user)
    }

    // --- Artists / songs browsing ---

    pub async fn artists(&self, start_index: u32, limit: u32) -> AppResult<Page<ArtistDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Artists/AlbumArtists",
                &[
                    ("userId", user_id),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Ascending".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(ArtistDto::from_item)
                .collect(),
        })
    }

    pub async fn artist(&self, artist_id: &str) -> AppResult<ArtistDto> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("ids", artist_id.to_string()),
                    ("fields", "Overview".into()),
                    ("enableImages", "true".into()),
                ],
            )
            .await?;
        response
            .items
            .into_iter()
            .next()
            .map(ArtistDto::from_item)
            .ok_or_else(|| AppError::Other(format!("artist {artist_id} not found")))
    }

    /// Albums where this artist is the *album* artist ("Albums" section).
    pub async fn artist_albums(&self, artist_id: &str) -> AppResult<Vec<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("albumArtistIds", artist_id.to_string()),
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "ProductionYear,SortName".into()),
                    // One order per key: a lone order applies to both.
                    ("sortOrder", "Descending,Ascending".into()),
                    ("fields", "ChildCount".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(AlbumDto::from_item)
            .collect())
    }

    /// Albums the artist merely appears on (any track credits them). The
    /// caller subtracts the album-artist albums to get the "Appears On" set.
    pub async fn artist_appears_on(&self, artist_id: &str) -> AppResult<Vec<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("artistIds", artist_id.to_string()),
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "ProductionYear,SortName".into()),
                    // One order per key: a lone order applies to both.
                    ("sortOrder", "Descending,Ascending".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(AlbumDto::from_item)
            .collect())
    }

    /// The artist's most-played tracks.
    pub async fn artist_top_songs(&self, artist_id: &str, limit: u32) -> AppResult<Vec<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("artistIds", artist_id.to_string()),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "PlayCount,SortName".into()),
                    // One order per key: a lone order applies to both.
                    ("sortOrder", "Descending,Ascending".into()),
                    ("filters", "IsPlayed".into()),
                    ("limit", limit.to_string()),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(TrackDto::from_item)
            .collect())
    }

    /// Recently played tracks (most recent `DatePlayed` first).
    pub async fn recently_played_tracks(&self, limit: u32) -> AppResult<Vec<TrackDto>> {
        self.played_tracks("DatePlayed", limit).await
    }

    /// Most played tracks (highest `PlayCount` first).
    pub async fn most_played_tracks(&self, limit: u32) -> AppResult<Vec<TrackDto>> {
        self.played_tracks("PlayCount", limit).await
    }

    async fn played_tracks(&self, sort_by: &str, limit: u32) -> AppResult<Vec<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("sortBy", sort_by.to_string()),
                    ("sortOrder", "Descending".into()),
                    ("filters", "IsPlayed".into()),
                    ("limit", limit.to_string()),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(TrackDto::from_item)
            .collect())
    }

    pub async fn songs(&self, start_index: u32, limit: u32) -> AppResult<Page<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Ascending".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(TrackDto::from_item)
                .collect(),
        })
    }

    pub async fn search_artists(&self, term: &str, limit: u32) -> AppResult<Vec<ArtistDto>> {
        Ok(self.search_artists_page(term, 0, limit).await?.items)
    }

    pub async fn search_artists_page(
        &self,
        term: &str,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<ArtistDto>> {
        let user_id = self.user_id()?.to_string();
        // Album artists only, consistent with the browse page — /Artists also
        // returns track-level contributors whose artist page would be empty.
        let response: ItemsResponse = self
            .get_json(
                "/Artists/AlbumArtists",
                &[
                    ("userId", user_id),
                    ("searchTerm", term.to_string()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(ArtistDto::from_item)
                .collect(),
        })
    }

    /// Server-generated mix seeded by any item (song, album, artist, genre).
    pub async fn instant_mix(&self, item_id: &str, limit: u32) -> AppResult<Vec<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                &format!("/Items/{item_id}/InstantMix"),
                &[
                    ("userId", user_id),
                    ("limit", limit.to_string()),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(TrackDto::from_item)
            .collect())
    }

    /// A cheap fingerprint of the music library: the newest track's
    /// `DateCreated` plus the total track count. It changes when music is
    /// added (newer date and/or higher count) or removed (lower count), which
    /// is all the change watcher needs. One `limit=1` request.
    pub async fn library_signature(&self) -> AppResult<LibrarySignature> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "DateCreated".into()),
                    ("sortOrder", "Descending".into()),
                    ("limit", "1".into()),
                    ("fields", "DateCreated".into()),
                    ("enableImages", "false".into()),
                    ("enableTotalRecordCount", "true".into()),
                ],
            )
            .await?;
        Ok(LibrarySignature {
            newest: response
                .items
                .into_iter()
                .next()
                .and_then(|i| i.date_created)
                .unwrap_or_default(),
            count: response.total_record_count,
        })
    }

    pub async fn tracks_by_ids(&self, ids: &[String]) -> AppResult<Vec<TrackDto>> {
        if ids.is_empty() {
            // 'ids=' would make the server ignore the by-ids path entirely
            // and return the user's library root folders instead.
            return Ok(Vec::new());
        }
        let user_id = self.user_id()?.to_string();
        let mut by_id: std::collections::HashMap<String, TrackDto> =
            std::collections::HashMap::with_capacity(ids.len());
        // Chunked to keep the request line short. Reads only, so repeating
        // the whole call (a re-login retry) is harmless.
        for chunk in unique_ids(ids).chunks(MAX_IDS_PER_REQUEST) {
            let response: ItemsResponse = self
                .get_json(
                    "/Items",
                    &[
                        ("userId", user_id.clone()),
                        ("ids", chunk.join(",")),
                        ("includeItemTypes", "Audio".into()),
                        ("fields", "NormalizationGain,Genres".into()),
                        ("enableImages", "true".into()),
                        ("imageTypeLimit", "1".into()),
                        ("enableUserData", "true".into()),
                    ],
                )
                .await?;
            by_id.extend(
                response
                    .items
                    .into_iter()
                    .map(TrackDto::from_item)
                    .map(|t| (t.id.clone(), t)),
            );
        }
        // The `ids=` endpoint returns SortName order, not the requested order —
        // restore the caller's selection order (e.g. a multi-select enqueue),
        // across all chunks.
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    pub async fn search_albums(&self, term: &str, limit: u32) -> AppResult<Vec<AlbumDto>> {
        Ok(self.search_albums_page(term, 0, limit).await?.items)
    }

    pub async fn search_albums_page(
        &self,
        term: &str,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("searchTerm", term.to_string()),
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("recursive", "true".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(AlbumDto::from_item)
                .collect(),
        })
    }

    pub async fn search_tracks(&self, term: &str, limit: u32) -> AppResult<Vec<TrackDto>> {
        Ok(self.search_tracks_page(term, 0, limit).await?.items)
    }

    pub async fn search_tracks_page(
        &self,
        term: &str,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("searchTerm", term.to_string()),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(TrackDto::from_item)
                .collect(),
        })
    }

    /// Universal audio endpoint: the server picks direct play when the client
    /// supports the container (Symphonia: flac/mp3/vorbis/wav, our libopus
    /// path: ogg-opus), otherwise transcodes to Ogg-Opus (progressive; HLS is
    /// a later refinement).
    ///
    /// Without a `playSessionId`: that one belongs to an attempt at playing
    /// the track, not to the track, and is appended when a source is opened
    /// (`player/source.rs`). It is what the server correlates our playback
    /// reports with its transcode session by, so skipping a track kills its
    /// ffmpeg job.
    pub fn stream_url(&self, item_id: &str) -> AppResult<String> {
        let user_id = self.user_id()?;
        Ok(format!(
            "{}/Audio/{item_id}/universal?userId={user_id}&deviceId={}&container=opus,ogg%7Copus,flac,mp3,ogg%7Cvorbis,wav&transcodingContainer=opus&transcodingProtocol=http&audioCodec=opus&maxStreamingBitrate=320000000",
            self.base_url, self.device_id
        ))
    }

    pub fn image_url(&self, item_id: &str, tag: &str, size: u32) -> String {
        format!(
            "{}/Items/{item_id}/Images/Primary?fillWidth={size}&fillHeight={size}&quality=90&tag={tag}",
            self.base_url
        )
    }

    /// Raw image fetch used by the jfimg:// protocol handler.
    pub async fn fetch_image(&self, item_id: &str, tag: &str, size: u32) -> AppResult<Vec<u8>> {
        let response = self
            .http
            .get(self.image_url(item_id, tag, size))
            .header("Authorization", self.auth_header())
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: "image fetch failed".into(),
            });
        }
        read_capped(response, MAX_IMAGE_BYTES, "cover image").await
    }

    /// Lyrics for a track. Servers answer 404 when the item has none (or no
    /// lyrics provider is installed) — that is an empty result, not an error.
    pub async fn lyrics(&self, item_id: &str) -> AppResult<LyricsDto> {
        let url = format!("{}/Audio/{item_id}/Lyrics", self.base_url);
        let response = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(LyricsDto::empty());
        }
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        let wire: LyricsResponse = response.json().await?;
        Ok(LyricsDto::from_wire(wire))
    }

    // --- Discovery (genre filter, alphabet scrubber, similar artists) ---

    /// Music genres, for the album-grid filter chips.
    pub async fn genres(&self) -> AppResult<Vec<GenreDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Genres",
                &[
                    ("userId", user_id),
                    // Music genres only — a mixed server also has movie/show
                    // genres that would filter the album grid to nothing.
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("sortBy", "SortName".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(GenreDto::from_item)
            .collect())
    }

    /// How many items sort strictly before `letter` — i.e. the index of the
    /// first item at/after that letter. `limit=0` makes the server return
    /// only `TotalRecordCount`, no items.
    pub async fn items_before_letter(
        &self,
        kind: &str,
        letter: &str,
        genre_id: Option<&str>,
    ) -> AppResult<i64> {
        let user_id = self.user_id()?.to_string();
        let mut query = vec![
            ("userId", user_id),
            ("nameLessThan", letter.to_string()),
            ("limit", "0".to_string()),
            ("enableImages", "false".to_string()),
        ];
        let path = if kind == "artists" {
            "/Artists/AlbumArtists"
        } else {
            query.push(("includeItemTypes", "MusicAlbum".into()));
            query.push(("recursive", "true".into()));
            "/Items"
        };
        if let Some(genre_id) = genre_id {
            query.push(("genreIds", genre_id.to_string()));
        }
        let response: ItemsResponse = self.get_json(path, &query).await?;
        Ok(response.total_record_count)
    }

    pub async fn similar_artists(&self, artist_id: &str, limit: u32) -> AppResult<Vec<ArtistDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                &format!("/Artists/{artist_id}/Similar"),
                &[
                    ("userId", user_id),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(ArtistDto::from_item)
            .collect())
    }

    /// Random album cards for the Discover hub. Random ordering is intentionally
    /// refreshed on every request; pagination is therefore a fresh sample.
    pub async fn discover_random_albums(
        &self,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<AlbumDto>> {
        self.albums(start_index, limit, None, "random", false, "all")
            .await
    }

    /// Random audio items that can seed Jellyfin's InstantMix endpoint.
    pub async fn discover_mix_seeds(
        &self,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "Random".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(TrackDto::from_item)
                .collect(),
        })
    }

    /// Pick an album artist to act as the explicit seed for a Similar row.
    pub async fn discover_artist_seed(&self) -> AppResult<Option<ArtistDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Artists/AlbumArtists",
                &[
                    ("userId", user_id),
                    ("sortBy", "Random".into()),
                    ("limit", "1".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        Ok(response.items.into_iter().next().map(ArtistDto::from_item))
    }

    /// Music years exposed by Jellyfin's dedicated Years endpoint.
    pub async fn music_years(&self) -> AppResult<Vec<i32>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Years",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Descending".into()),
                    ("enableImages", "false".into()),
                ],
            )
            .await?;
        let mut years = response
            .items
            .into_iter()
            .filter_map(|item| item.name.parse::<i32>().ok())
            .filter(|year| (1..=9999).contains(year))
            .collect::<Vec<_>>();
        years.sort_unstable();
        years.dedup();
        years.reverse();
        Ok(years)
    }

    pub async fn decade_albums(
        &self,
        start_year: i32,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<AlbumDto>> {
        if !(1..=9990).contains(&start_year) || start_year % 10 != 0 {
            return Err(AppError::Other(
                "startYear must be a decade between 10 and 9990".into(),
            ));
        }
        let user_id = self.user_id()?.to_string();
        let years = (start_year..=start_year + 9)
            .map(|year| year.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("recursive", "true".into()),
                    ("years", years),
                    ("sortBy", "ProductionYear,SortName".into()),
                    ("sortOrder", "Descending,Ascending".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(AlbumDto::from_item)
                .collect(),
        })
    }

    /// Played tracks ordered by oldest DatePlayed, then mapped to unique albums.
    /// Play history belongs to tracks in Jellyfin, so querying BoxSet/Album
    /// user-data here would silently produce the wrong result.
    pub async fn long_not_heard_albums(
        &self,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let mut offset = 0u32;
        let mut items = Vec::new();
        loop {
            let response: ItemsResponse = self
                .get_json(
                    "/Items",
                    &[
                        ("userId", user_id.clone()),
                        ("includeItemTypes", "Audio".into()),
                        ("recursive", "true".into()),
                        ("filters", "IsPlayed".into()),
                        ("sortBy", "DatePlayed".into()),
                        ("sortOrder", "Ascending".into()),
                        ("startIndex", offset.to_string()),
                        ("limit", SMART_SCAN_BATCH.to_string()),
                        ("enableImages", "false".into()),
                        ("enableUserData", "true".into()),
                    ],
                )
                .await?;
            let received = response.items.len() as u32;
            items.extend(response.items);
            offset = offset.saturating_add(received);
            // `total_record_count` is a server-supplied number and the only
            // other stop condition, so bound the scan independently: a server
            // that keeps returning full pages against a huge count would
            // otherwise loop forever and grow `items` without limit. A partial
            // page also means the end, whatever the count claims.
            if received == 0
                || received < SMART_SCAN_BATCH
                || items.len() >= SMART_SCAN_MAX_ITEMS
                || offset == u32::MAX
                || i64::from(offset) >= response.total_record_count
            {
                if items.len() >= SMART_SCAN_MAX_ITEMS {
                    tracing::warn!(
                        "smart scan stopped at {} items; results are partial",
                        items.len()
                    );
                }
                break;
            }
        }
        let album_ids = unique_album_ids(items);
        let total = album_ids.len() as i64;
        let selected = album_ids
            .into_iter()
            .skip(start_index as usize)
            .take(limit as usize)
            .collect::<Vec<_>>();
        Ok(Page {
            items: self.albums_by_ids(&selected).await?,
            total,
        })
    }

    /// The `/Items` query behind every smart view: the server-side part of the
    /// filter, newest tracks first. Shared by the paged request and the scan,
    /// so both see the same order.
    fn smart_items_query(
        &self,
        plan: &SmartQueryPlan,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Vec<(&'static str, String)>> {
        let mut query = vec![
            ("userId", self.user_id()?.to_string()),
            ("includeItemTypes", "Audio".into()),
            ("recursive", "true".into()),
            ("sortBy", "DateCreated,SortName".into()),
            ("sortOrder", "Descending,Ascending".into()),
            ("startIndex", start_index.to_string()),
            ("limit", limit.to_string()),
            ("fields", "DateCreated,NormalizationGain,Genres".into()),
            ("enableImages", "true".into()),
            ("imageTypeLimit", "1".into()),
            ("enableUserData", "true".into()),
        ];
        if let Some(filters) = &plan.filters {
            query.push(("filters", filters.clone()));
        }
        if let Some(genre_ids) = &plan.genre_ids {
            query.push(("genreIds", genre_ids.clone()));
        }
        if let Some(years) = &plan.years {
            query.push(("years", years.clone()));
        }
        Ok(query)
    }

    async fn smart_track_items(&self, filter: &SmartFilter) -> AppResult<Vec<BaseItem>> {
        let plan = smart_query_plan(filter)?;
        let mut offset = 0u32;
        let mut scanned = 0usize;
        let mut matches = Vec::new();
        loop {
            let query = self.smart_items_query(&plan, offset, SMART_SCAN_BATCH)?;
            let response: ItemsResponse = self.get_json("/Items", &query).await?;
            let received = response.items.len() as u32;
            let past_cutoff = filter
                .added_since
                .as_deref()
                .is_some_and(|since| reached_added_cutoff(&response.items, since));
            matches.extend(
                response
                    .items
                    .into_iter()
                    .filter(|item| smart_item_matches(item, filter)),
            );
            offset = offset.saturating_add(received);
            // Same bounds as `long_not_heard_albums`: `total_record_count` is
            // a server-supplied number and must not be the only stop
            // condition. Count *scanned* items, not matched ones — a selective
            // filter would otherwise leave the ceiling unreachable.
            scanned = scanned.saturating_add(received as usize);
            if received == 0
                || received < SMART_SCAN_BATCH
                || past_cutoff
                || scanned >= SMART_SCAN_MAX_ITEMS
                || offset == u32::MAX
                || i64::from(offset) >= response.total_record_count
            {
                if scanned >= SMART_SCAN_MAX_ITEMS && !past_cutoff {
                    tracing::warn!("smart scan stopped at {scanned} items; results are partial");
                }
                break;
            }
        }
        Ok(matches)
    }

    /// Every track of a smart view — for playing or materializing it, where
    /// the whole result is needed anyway.
    pub async fn smart_tracks(&self, filter: &SmartFilter) -> AppResult<Vec<TrackDto>> {
        Ok(self
            .smart_track_items(filter)
            .await?
            .into_iter()
            .map(TrackDto::from_item)
            .collect())
    }

    /// One page of a smart view, for browsing it.
    ///
    /// When the server can evaluate the whole filter, it pages too: one
    /// request per page. Otherwise only a scan knows which tracks match; the
    /// first page scans, and later pages fetch their slice by id from the ids
    /// that scan found, instead of scanning the library again for every page.
    pub async fn smart_tracks_page(
        &self,
        filter: &SmartFilter,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<TrackDto>> {
        let plan = smart_query_plan(filter)?;
        if smart_filter_is_server_side(filter) {
            let query = self.smart_items_query(&plan, start_index, limit)?;
            let response: ItemsResponse = self.get_json("/Items", &query).await?;
            return Ok(Page {
                items: response
                    .items
                    .into_iter()
                    .map(TrackDto::from_item)
                    .collect(),
                total: response.total_record_count,
            });
        }

        if start_index > 0 {
            let cached = self
                .smart_scan_cache
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|cache| cache.ids_for(filter, Instant::now(), SMART_SCAN_CACHE_TTL));
            if let Some(ids) = cached {
                let selected = ids
                    .iter()
                    .skip(start_index as usize)
                    .take(limit as usize)
                    .cloned()
                    .collect::<Vec<_>>();
                return Ok(Page {
                    items: self.tracks_by_ids(&selected).await?,
                    total: ids.len() as i64,
                });
            }
        }

        let items = self.smart_track_items(filter).await?;
        let ids = Arc::new(items.iter().map(|item| item.id.clone()).collect::<Vec<_>>());
        let total = ids.len() as i64;
        *self.smart_scan_cache.lock().unwrap() = Some(ScanCache {
            key: filter.clone(),
            scanned_at: Instant::now(),
            ids,
        });
        Ok(Page {
            items: items
                .into_iter()
                .skip(start_index as usize)
                .take(limit as usize)
                .map(TrackDto::from_item)
                .collect(),
            total,
        })
    }

    /// Resolve albums in caller order and preserve disc/track order inside
    /// every album. Used by Discover row playback and collection playback.
    pub async fn tracks_for_albums(&self, album_ids: &[String]) -> AppResult<Vec<TrackDto>> {
        let mut per_album = Vec::with_capacity(album_ids.len());
        for album_id in album_ids {
            if album_id.trim().is_empty() {
                return Err(AppError::Other("album id must not be empty".into()));
            }
            per_album.push(self.album_tracks(album_id).await?);
        }
        Ok(flatten_album_tracks(per_album))
    }

    // --- Read-only Jellyfin Collections / BoxSets ---

    pub async fn collections(
        &self,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<CollectionDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "BoxSet".into()),
                    ("recursive", "true".into()),
                    ("collapseBoxSetItems", "false".into()),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Ascending".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("fields", "ChildCount".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(CollectionDto::from_item)
                .collect(),
        })
    }

    pub async fn collection(&self, collection_id: &str) -> AppResult<CollectionDto> {
        if collection_id.trim().is_empty() {
            return Err(AppError::Other("collection id must not be empty".into()));
        }
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("ids", collection_id.to_string()),
                    ("includeItemTypes", "BoxSet".into()),
                    ("fields", "ChildCount".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        response
            .items
            .into_iter()
            .next()
            .map(CollectionDto::from_item)
            .ok_or_else(|| AppError::Other(format!("collection {collection_id} not found")))
    }

    pub async fn collection_albums(&self, collection_id: &str) -> AppResult<Vec<AlbumDto>> {
        if collection_id.trim().is_empty() {
            return Err(AppError::Other("collection id must not be empty".into()));
        }
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("parentId", collection_id.to_string()),
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("recursive", "true".into()),
                    ("collapseBoxSetItems", "false".into()),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Ascending".into()),
                    ("fields", "ChildCount".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(AlbumDto::from_item)
            .collect())
    }

    pub async fn collection_tracks(&self, collection_id: &str) -> AppResult<Vec<TrackDto>> {
        let album_ids = self
            .collection_albums(collection_id)
            .await?
            .into_iter()
            .map(|album| album.id)
            .collect::<Vec<_>>();
        self.tracks_for_albums(&album_ids).await
    }

    // --- Library slice: favorites, playlists, home rows ---

    /// Fire a bodyless request (POST/DELETE) and ignore the response body.
    async fn send_no_content(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> AppResult<()> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .http
            .request(method, &url)
            .header("Authorization", self.auth_header())
            .query(query)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        Ok(())
    }

    /// Invalidate this access token server-side. Best effort: signing out
    /// locally must still work when the server is unreachable, but when it is
    /// reachable the token should stop being usable rather than staying valid
    /// until it expires on its own.
    pub async fn logout(&self) -> AppResult<()> {
        self.send_no_content(reqwest::Method::POST, "/Sessions/Logout", &[])
            .await
    }

    /// Mark/unmark an item (track, album, ...) as favorite. The server
    /// answers with the new UserItemDataDto — we don't need it.
    pub async fn set_favorite(&self, item_id: &str, favorite: bool) -> AppResult<()> {
        let user_id = self.user_id()?.to_string();
        let method = if favorite {
            reqwest::Method::POST
        } else {
            reqwest::Method::DELETE
        };
        self.send_no_content(
            method,
            &format!("/UserFavoriteItems/{item_id}"),
            &[("userId", user_id)],
        )
        .await
    }

    pub async fn playlists(&self) -> AppResult<Vec<PlaylistDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Playlist".into()),
                    // Music client: keep video/mixed playlists out.
                    ("mediaTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "SortName".into()),
                    // ChildCount (the track count) is only sent when asked for.
                    ("fields", "ChildCount".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            .map(PlaylistDto::from_item)
            .collect())
    }

    pub async fn playlist(&self, playlist_id: &str) -> AppResult<PlaylistDto> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("ids", playlist_id.to_string()),
                    ("fields", "ChildCount".into()),
                    ("enableImages", "true".into()),
                ],
            )
            .await?;
        response
            .items
            .into_iter()
            .next()
            .map(PlaylistDto::from_item)
            .ok_or_else(|| AppError::Other(format!("playlist {playlist_id} not found")))
    }

    pub async fn playlist_tracks(&self, playlist_id: &str) -> AppResult<Vec<PlaylistTrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                &format!("/Playlists/{playlist_id}/Items"),
                &[
                    ("userId", user_id),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(response
            .items
            .into_iter()
            // Mixed playlists can contain video entries; those must never
            // reach the audio pipeline.
            .filter(|item| item.item_type == "Audio")
            .map(PlaylistTrackDto::from_item)
            .collect())
    }

    /// Fetch the current source playlist and resolve exact PlaylistItemIds to
    /// the item ids accepted by Jellyfin's playlist-add endpoint.
    pub async fn playlist_item_ids_for_entries(
        &self,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> AppResult<Vec<String>> {
        if playlist_id.trim().is_empty() {
            return Err(AppError::Other(
                "source playlist id must not be empty".into(),
            ));
        }

        // Validate the selection before doing a network round-trip, then use a
        // fresh response to guard against stale entry ids from an older UI view.
        validate_playlist_entry_ids(entry_ids)?;
        let tracks = self.playlist_tracks(playlist_id).await?;
        resolve_playlist_item_ids(
            tracks
                .iter()
                .map(|entry| (entry.entry_id.as_str(), entry.track.id.as_str())),
            entry_ids,
        )
    }

    /// Duplicate a playlist from a fresh source snapshot. Item ids are kept in
    /// source order, including repeated tracks with distinct playlist entries.
    pub async fn duplicate_playlist(
        &self,
        source_playlist_id: &str,
        name: &str,
    ) -> AppResult<String> {
        if source_playlist_id.trim().is_empty() {
            return Err(AppError::Other(
                "source playlist id must not be empty".into(),
            ));
        }
        if name.trim().is_empty() {
            return Err(AppError::Other("playlist name must not be empty".into()));
        }

        let track_ids = self
            .playlist_tracks(source_playlist_id)
            .await?
            .into_iter()
            .map(|entry| entry.track.id)
            .collect::<Vec<_>>();
        self.create_playlist(name, &track_ids).await
    }

    pub async fn create_playlist(&self, name: &str, track_ids: &[String]) -> AppResult<String> {
        let user_id = self.user_id()?.to_string();
        let url = format!("{}/Playlists", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({
                "Name": name,
                "Ids": track_ids,
                "UserId": user_id,
                "MediaType": "Audio",
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        let created: CreatePlaylistResult = response.json().await?;
        Ok(created.id)
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> AppResult<()> {
        self.send_no_content(
            reqwest::Method::DELETE,
            &format!("/Items/{playlist_id}"),
            &[],
        )
        .await
    }

    /// Permanently delete items on the server — the files are removed from
    /// disk (Jellyfin has no trash). Works for tracks and whole albums
    /// (deleting an album item removes its folder). Requires the signed-in
    /// user to hold a content-deletion permission, else the server answers
    /// 401/403.
    ///
    /// Chunked to keep the request line short; safe to repeat as a whole (see
    /// [`Self::delete_items_chunk`]).
    pub async fn delete_items(&self, ids: &[String]) -> AppResult<()> {
        for chunk in unique_ids(ids).chunks(MAX_IDS_PER_REQUEST) {
            self.delete_items_chunk(chunk).await?;
        }
        Ok(())
    }

    /// One `DELETE /Items` request. Jellyfin deletes the ids in order and
    /// answers 404 at the first one that no longer exists, leaving the rest
    /// untouched. A repeated call sees exactly that — `with_retry` re-runs the
    /// whole deletion after a re-login, and an album deleted earlier in the
    /// same selection takes its tracks with it — so on a 404 whatever still
    /// exists is deleted instead of failing the operation, round by round
    /// (the retry itself can remove an album before one of its own tracks).
    async fn delete_items_chunk(&self, ids: &[&str]) -> AppResult<()> {
        let mut pending = ids.to_vec();
        loop {
            let message = match self
                .send_no_content(
                    reqwest::Method::DELETE,
                    "/Items",
                    &[("ids", pending.join(","))],
                )
                .await
            {
                Err(AppError::Server {
                    status: 404,
                    message,
                }) => message,
                other => return other,
            };
            let existing = self.existing_item_ids(&pending).await?;
            let remaining = pending
                .iter()
                .copied()
                .filter(|id| existing.contains(*id))
                .collect::<Vec<_>>();
            if remaining.len() == pending.len() {
                // Nothing was missing, so the 404 was about something else.
                return Err(AppError::Server {
                    status: 404,
                    message,
                });
            }
            if remaining.is_empty() {
                return Ok(());
            }
            // Every round drops at least one id, so this ends.
            pending = remaining;
        }
    }

    /// Which of `ids` (at most [`MAX_IDS_PER_REQUEST`]) the server still has.
    async fn existing_item_ids(&self, ids: &[&str]) -> AppResult<HashSet<String>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("ids", ids.join(",")),
                    ("enableImages", "false".into()),
                    ("enableUserData", "false".into()),
                ],
            )
            .await?;
        Ok(response.items.into_iter().map(|item| item.id).collect())
    }

    /// Start a streaming download of the original media file. The caller owns
    /// the response body and can write chunks directly to disk without keeping
    /// the complete track in memory.
    pub async fn download_item_stream(
        &self,
        item_id: &str,
    ) -> AppResult<(reqwest::Response, String)> {
        let url = format!("{}/Items/{item_id}/Download", self.base_url);
        let response = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .timeout(Duration::from_secs(6 * 60 * 60))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        let filename = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(content_disposition_filename)
            .unwrap_or_else(|| item_id.to_string());
        Ok((response, filename))
    }

    /// Rename a playlist. Jellyfin's UpdatePlaylistDto only touches the fields
    /// present, so a Name-only body leaves the tracks intact (10.8+).
    pub async fn rename_playlist(&self, playlist_id: &str, name: &str) -> AppResult<()> {
        self.post_no_content(
            &format!("/Playlists/{playlist_id}"),
            serde_json::json!({ "Name": name }),
        )
        .await
    }

    /// Add items to a playlist in ONE request, so at most
    /// [`MAX_IDS_PER_REQUEST`] of them. Adding is not idempotent: callers split
    /// a longer list with `chunks(MAX_IDS_PER_REQUEST)` and give every chunk its
    /// own `with_retry`, so a re-login retry never re-sends a chunk the server
    /// already applied.
    pub async fn playlist_add_chunk(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> AppResult<()> {
        if track_ids.len() > MAX_IDS_PER_REQUEST {
            return Err(AppError::Other(format!(
                "at most {MAX_IDS_PER_REQUEST} tracks per playlist add request"
            )));
        }
        if track_ids.is_empty() {
            return Ok(());
        }
        let user_id = self.user_id()?.to_string();
        self.send_no_content(
            reqwest::Method::POST,
            &format!("/Playlists/{playlist_id}/Items"),
            &[("ids", track_ids.join(",")), ("userId", user_id)],
        )
        .await
    }

    /// Remove playlist entries. `entry_ids` are PlaylistItemIds, NOT item ids.
    /// Chunked to keep the request line short; safe to repeat as a whole,
    /// because Jellyfin ignores entry ids that are no longer in the playlist.
    pub async fn playlist_remove(&self, playlist_id: &str, entry_ids: &[String]) -> AppResult<()> {
        for chunk in entry_ids.chunks(MAX_IDS_PER_REQUEST) {
            self.send_no_content(
                reqwest::Method::DELETE,
                &format!("/Playlists/{playlist_id}/Items"),
                &[("entryIds", chunk.join(","))],
            )
            .await?;
        }
        Ok(())
    }

    /// Move a playlist entry (by PlaylistItemId) to `new_index`
    /// (remove-then-insert semantics, like the queue).
    pub async fn playlist_move(
        &self,
        playlist_id: &str,
        entry_id: &str,
        new_index: usize,
    ) -> AppResult<()> {
        self.send_no_content(
            reqwest::Method::POST,
            &format!("/Playlists/{playlist_id}/Items/{entry_id}/Move/{new_index}"),
            &[],
        )
        .await
    }

    /// Album row for the home page: newest/most/last played depending on
    /// `sort_by` ("DateCreated" / "PlayCount" / "DatePlayed").
    ///
    /// Playback only updates Played/PlayCount/DatePlayed on the *track*
    /// user-data, never on the album — so the played-based rows query tracks
    /// and map them to their albums (jellyfin-web does the same).
    pub async fn home_albums(&self, sort_by: &str, limit: u32) -> AppResult<Vec<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        if sort_by == "DateCreated" {
            let response: ItemsResponse = self
                .get_json(
                    "/Items",
                    &[
                        ("userId", user_id),
                        ("includeItemTypes", "MusicAlbum".into()),
                        ("recursive", "true".into()),
                        ("sortBy", sort_by.to_string()),
                        ("sortOrder", "Descending".into()),
                        ("limit", limit.to_string()),
                        ("enableImages", "true".into()),
                        ("imageTypeLimit", "1".into()),
                        ("enableUserData", "true".into()),
                    ],
                )
                .await?;
            return Ok(response
                .items
                .into_iter()
                .map(AlbumDto::from_item)
                .collect());
        }

        let mut query = vec![
            ("userId", user_id.clone()),
            ("includeItemTypes", "Audio".into()),
            ("recursive", "true".into()),
            ("sortBy", sort_by.to_string()),
            ("sortOrder", "Descending".into()),
            // Oversample: many top tracks share an album.
            ("limit", (limit * 8).to_string()),
            ("enableImages", "false".into()),
            ("enableUserData", "true".into()),
        ];
        if sort_by == "DatePlayed" {
            // Never-played tracks have no DatePlayed; keep them out.
            query.push(("filters", "IsPlayed".into()));
        }
        let response: ItemsResponse = self.get_json("/Items", &query).await?;
        let mut album_ids: Vec<String> = Vec::new();
        for item in response.items {
            if let Some(album_id) = item.album_id {
                if !album_ids.contains(&album_id) {
                    album_ids.push(album_id);
                    if album_ids.len() as u32 >= limit {
                        break;
                    }
                }
            }
        }
        self.albums_by_ids(&album_ids).await
    }

    /// Fetch albums by id, preserving the given order (the `ids` query returns
    /// arbitrary order).
    async fn albums_by_ids(&self, album_ids: &[String]) -> AppResult<Vec<AlbumDto>> {
        if album_ids.is_empty() {
            return Ok(Vec::new());
        }
        let user_id = self.user_id()?.to_string();
        let mut by_id: std::collections::HashMap<String, AlbumDto> =
            std::collections::HashMap::with_capacity(album_ids.len());
        for chunk in unique_ids(album_ids).chunks(MAX_IDS_PER_REQUEST) {
            let response: ItemsResponse = self
                .get_json(
                    "/Items",
                    &[
                        ("userId", user_id.clone()),
                        ("ids", chunk.join(",")),
                        ("enableImages", "true".into()),
                        ("imageTypeLimit", "1".into()),
                        ("enableUserData", "true".into()),
                    ],
                )
                .await?;
            by_id.extend(
                response
                    .items
                    .into_iter()
                    .map(AlbumDto::from_item)
                    .map(|a| (a.id.clone(), a)),
            );
        }
        Ok(album_ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// "Forgotten favorites": favorited tracks you've played but least recently,
    /// mapped up to their albums (played favorites, oldest DatePlayed first).
    pub async fn forgotten_favorite_albums(&self, limit: u32) -> AppResult<Vec<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("filters", "IsFavorite,IsPlayed".into()),
                    ("sortBy", "DatePlayed".into()),
                    ("sortOrder", "Ascending".into()),
                    ("limit", (limit * 8).to_string()),
                    ("enableImages", "false".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        let mut album_ids: Vec<String> = Vec::new();
        for item in response.items {
            if let Some(album_id) = item.album_id {
                if !album_ids.contains(&album_id) {
                    album_ids.push(album_id);
                    if album_ids.len() as u32 >= limit {
                        break;
                    }
                }
            }
        }
        self.albums_by_ids(&album_ids).await
    }

    /// One page of the favorited tracks (heart set), by name. Jellyfin stores
    /// no date for when an item was favorited, so there is no "recently
    /// favorited" order to offer.
    pub async fn favorite_tracks(&self, start_index: u32, limit: u32) -> AppResult<Page<TrackDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("filters", "IsFavorite".into()),
                    ("includeItemTypes", "Audio".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Ascending".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("fields", "NormalizationGain,Genres".into()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(TrackDto::from_item)
                .collect(),
        })
    }

    /// One page of the favorited albums, by name.
    pub async fn favorite_albums(&self, start_index: u32, limit: u32) -> AppResult<Page<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("filters", "IsFavorite".into()),
                    ("includeItemTypes", "MusicAlbum".into()),
                    ("recursive", "true".into()),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Ascending".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(AlbumDto::from_item)
                .collect(),
        })
    }

    /// One page of the favorited artists, by name.
    ///
    /// Through `/Artists`, like jellyfin-web's favorites page: in 10.11 an
    /// `/Items` query for `MusicArtist` finds none, because artists have no top
    /// parent and the user's library restriction filters them out. `/Artists`
    /// only counts `TotalRecordCount` when `limit` is passed — every page does.
    pub async fn favorite_artists(
        &self,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<ArtistDto>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/Artists",
                &[
                    ("userId", user_id),
                    ("filters", "IsFavorite".into()),
                    ("sortBy", "SortName".into()),
                    ("sortOrder", "Ascending".into()),
                    ("startIndex", start_index.to_string()),
                    ("limit", limit.to_string()),
                    ("enableImages", "true".into()),
                    ("imageTypeLimit", "1".into()),
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        Ok(Page {
            total: response.total_record_count,
            items: response
                .items
                .into_iter()
                .map(ArtistDto::from_item)
                .collect(),
        })
    }

    /// Technical audio details (codec/bitrate/…) for the "Info" panel.
    pub async fn track_info(&self, item_id: &str) -> AppResult<TrackInfoDto> {
        let user_id = self.user_id()?.to_string();
        let resp: MediaSourcesResponse = self
            .get_json(
                "/Items",
                &[
                    ("userId", user_id),
                    ("ids", item_id.to_string()),
                    ("fields", "MediaSources,Path".into()),
                ],
            )
            .await?;
        let source = resp
            .items
            .into_iter()
            .next()
            .and_then(|i| i.media_sources.into_iter().next())
            .ok_or_else(|| AppError::Other(format!("no media source for {item_id}")))?;
        Ok(TrackInfoDto::from_source(source))
    }
}

#[cfg(test)]
mod playlist_selection_tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn selected_entries_keep_source_order_and_repeated_tracks() {
        let source = [
            ("entry-1", "track-repeated"),
            ("entry-2", "track-other"),
            ("entry-3", "track-repeated"),
            ("entry-4", "track-unselected"),
        ];
        let selected = ids(&["entry-3", "entry-2", "entry-1"]);

        let resolved = resolve_playlist_item_ids(source, &selected).expect("valid selection");

        assert_eq!(
            resolved,
            ids(&["track-repeated", "track-other", "track-repeated"])
        );
    }

    #[test]
    fn empty_entry_selections_are_rejected() {
        let source = [("entry-1", "track-1")];

        let empty = resolve_playlist_item_ids(source, &[]).unwrap_err();
        assert_eq!(empty.to_string(), "no playlist entries selected");

        let blank = resolve_playlist_item_ids(source, &ids(&["  "])).unwrap_err();
        assert_eq!(blank.to_string(), "playlist entry id must not be empty");
    }

    #[test]
    fn unknown_and_duplicate_entry_ids_are_rejected() {
        let source = [("entry-1", "track-1")];

        let unknown = resolve_playlist_item_ids(source, &ids(&["entry-1", "missing"])).unwrap_err();
        assert_eq!(
            unknown.to_string(),
            "playlist entries not found in source playlist: missing"
        );

        let duplicate =
            resolve_playlist_item_ids(source, &ids(&["entry-1", "entry-1"])).unwrap_err();
        assert_eq!(
            duplicate.to_string(),
            "duplicate playlist entry id: entry-1"
        );
    }
}

#[cfg(test)]
mod request_helper_tests {
    use super::*;

    fn filename(header: &str) -> Option<String> {
        content_disposition_filename(header)
    }

    #[test]
    fn content_disposition_prefers_the_extended_filename() {
        // What ASP.NET Core (Jellyfin) sends for a non-ASCII name.
        assert_eq!(
            filename(
                "attachment; filename=\"F_r Elise.flac\"; filename*=UTF-8''F%C3%BCr%20Elise.flac"
            )
            .as_deref(),
            Some("Für Elise.flac")
        );
        // Order and name case do not matter; the language tag is ignored.
        assert_eq!(
            filename("attachment; FILENAME*=utf-8'de'%C3%84rger.mp3; filename=_rger.mp3")
                .as_deref(),
            Some("Ärger.mp3")
        );
        assert_eq!(
            filename("attachment; filename*=ISO-8859-1''caf%E9.flac").as_deref(),
            Some("café.flac")
        );
    }

    #[test]
    fn content_disposition_falls_back_to_the_plain_filename() {
        for broken in [
            "UTF-8''%ZZ.flac",  // bad escape
            "UTF-8''%C3.flac",  // not UTF-8 after decoding
            "UTF-8''%C",        // truncated escape
            "KOI8-R''abc.flac", // unsupported charset
            "no-quotes-at-all", // not an ext-value
            "UTF-8''",          // empty
        ] {
            assert_eq!(
                filename(&format!(
                    "attachment; filename*={broken}; filename=\"fallback.flac\""
                ))
                .as_deref(),
                Some("fallback.flac"),
                "filename*={broken}"
            );
        }
    }

    #[test]
    fn content_disposition_quoted_values_are_parsed_whole() {
        assert_eq!(
            filename("attachment; filename=\"Love; Hate.flac\"").as_deref(),
            Some("Love; Hate.flac")
        );
        assert_eq!(
            filename(r#"attachment; filename="say \"hi\".flac"; size=12"#).as_deref(),
            Some(r#"say "hi".flac"#)
        );
        // A `filename=` inside another parameter's quoted value is not one.
        assert_eq!(
            filename(r#"attachment; title="a; filename=evil.exe"; filename="good.flac""#)
                .as_deref(),
            Some("good.flac")
        );
        assert_eq!(
            filename("attachment; filename=plain.flac").as_deref(),
            Some("plain.flac")
        );
        assert_eq!(
            filename("attachment;filename = spaced name.mp3 ;").as_deref(),
            Some("spaced name.mp3")
        );
        assert_eq!(filename("attachment"), None);
        assert_eq!(filename("attachment; filename=\"\""), None);
        assert_eq!(filename("inline; size=3"), None);
    }

    #[test]
    fn error_messages_are_collapsed_and_bounded() {
        assert_eq!(shorten_error_message("  Not found \n"), "Not found");
        assert_eq!(
            shorten_error_message("<html>\n  <body>\r\n\t502 Bad Gateway</body>"),
            "<html> <body> 502 Bad Gateway</body>"
        );
        let long = shorten_error_message(&"ä".repeat(5000));
        assert_eq!(long.chars().count(), MAX_ERROR_MESSAGE_CHARS + 1);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn error_body_is_read_only_up_to_the_cap() {
        let body = format!("{}{}", " x".repeat(10_000), "TAIL");
        let response = reqwest::Response::from(
            tauri::http::Response::builder()
                .status(502)
                .body(body.into_bytes())
                .unwrap(),
        );
        let message = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(error_message(response));
        assert!(message.chars().count() <= MAX_ERROR_MESSAGE_CHARS + 1);
        assert!(!message.contains("TAIL"));
    }

    #[test]
    fn id_chunks_fit_the_request_line_and_repeats_are_dropped() {
        let chunk = vec!["0123456789abcdef0123456789abcdef"; MAX_IDS_PER_REQUEST];
        // Kestrel's default request-line limit is 8 KB, other params included.
        assert!(chunk.join("%2C").len() < 4096);

        let ids = ["b", "a", "b", "c", "a"].map(String::from);
        assert_eq!(unique_ids(&ids), ["b", "a", "c"]);
    }
}

#[cfg(test)]
mod phase11_query_tests {
    use super::*;

    fn smart_filter() -> SmartFilter {
        SmartFilter {
            year_from: Some(1990),
            year_to: Some(1992),
            genre_ids: vec!["rock".into(), "indie".into()],
            played: PlayedFilter::Played,
            favorite_only: true,
            min_play_count: Some(3),
            added_since: Some("2024-02-29".into()),
        }
    }

    #[test]
    fn smart_filter_validates_ranges_ids_counts_and_dates() {
        assert!(validate_smart_filter(&smart_filter()).is_ok());

        let mut invalid = smart_filter();
        invalid.year_from = Some(2025);
        invalid.year_to = Some(2024);
        assert_eq!(
            validate_smart_filter(&invalid).unwrap_err().to_string(),
            "yearFrom must not be greater than yearTo"
        );

        let mut invalid = smart_filter();
        invalid.genre_ids.push("rock".into());
        assert_eq!(
            validate_smart_filter(&invalid).unwrap_err().to_string(),
            "duplicate genre id: rock"
        );

        let mut invalid = smart_filter();
        invalid.min_play_count = Some(-1);
        assert_eq!(
            validate_smart_filter(&invalid).unwrap_err().to_string(),
            "minPlayCount must not be negative"
        );

        let mut invalid = smart_filter();
        invalid.added_since = Some("2023-02-29".into());
        assert_eq!(
            validate_smart_filter(&invalid).unwrap_err().to_string(),
            "addedSince must be a valid YYYY-MM-DD date"
        );
    }

    #[test]
    fn smart_query_plan_combines_server_filters() {
        let plan = smart_query_plan(&smart_filter()).expect("valid query");
        assert_eq!(plan.filters.as_deref(), Some("IsFavorite,IsPlayed"));
        assert_eq!(plan.genre_ids.as_deref(), Some("rock,indie"));
        assert_eq!(plan.years.as_deref(), Some("1990,1991,1992"));
    }

    #[test]
    fn smart_local_predicates_apply_track_play_data_and_date() {
        let mut item = BaseItem {
            production_year: Some(1991),
            date_created: Some("2024-03-01T12:30:00.0000000Z".into()),
            genre_items: vec![NameGuidPair {
                id: "rock".into(),
                name: "Rock".into(),
            }],
            user_data: Some(UserDataWire {
                is_favorite: true,
                play_count: 3,
                ..UserDataWire::default()
            }),
            ..BaseItem::default()
        };
        assert!(smart_item_matches(&item, &smart_filter()));

        item.user_data.as_mut().expect("user data").play_count = 2;
        assert!(!smart_item_matches(&item, &smart_filter()));
        item.user_data.as_mut().expect("user data").play_count = 3;
        item.date_created = Some("2024-02-28T23:59:59Z".into());
        assert!(!smart_item_matches(&item, &smart_filter()));
    }

    #[test]
    fn only_filters_without_local_predicates_page_on_the_server() {
        let server_side = SmartFilter {
            year_from: Some(1990),
            year_to: Some(1999),
            genre_ids: vec!["rock".into()],
            played: PlayedFilter::Played,
            favorite_only: true,
            min_play_count: Some(0),
            added_since: None,
        };
        assert!(smart_filter_is_server_side(&server_side));
        assert!(smart_filter_is_server_side(&SmartFilter::default()));

        // Each of these has no server-side query parameter.
        for local in [
            SmartFilter {
                min_play_count: Some(1),
                ..server_side.clone()
            },
            SmartFilter {
                added_since: Some("2024-01-01".into()),
                ..server_side.clone()
            },
            SmartFilter {
                year_to: None,
                ..server_side.clone()
            },
            SmartFilter {
                year_from: None,
                ..server_side.clone()
            },
        ] {
            assert!(!smart_filter_is_server_side(&local), "{local:?}");
        }
    }

    #[test]
    fn added_since_cutoff_is_reached_by_the_first_older_track() {
        let created = |date: Option<&str>| BaseItem {
            date_created: date.map(str::to_string),
            ..BaseItem::default()
        };
        let newer = [
            created(Some("2024-03-02T08:00:00Z")),
            created(Some("2024-03-01T00:00:00Z")),
            // An unknown date says nothing about what follows.
            created(None),
        ];
        assert!(!reached_added_cutoff(&newer, "2024-03-01"));

        let crossing = [
            created(Some("2024-03-01T23:59:59Z")),
            created(Some("2024-02-29T23:59:59Z")),
        ];
        assert!(reached_added_cutoff(&crossing, "2024-03-01"));
    }

    #[test]
    fn scan_cache_serves_only_the_same_filter_within_its_ttl() {
        let scanned_at = Instant::now();
        let cache = ScanCache {
            key: smart_filter(),
            scanned_at,
            ids: Arc::new(vec!["a".into(), "b".into()]),
        };
        let ttl = SMART_SCAN_CACHE_TTL;
        let fresh = scanned_at + Duration::from_secs(10);
        assert_eq!(
            cache.ids_for(&smart_filter(), fresh, ttl).as_deref(),
            Some(&vec!["a".to_string(), "b".to_string()])
        );

        let mut other = smart_filter();
        other.min_play_count = Some(4);
        assert!(cache.ids_for(&other, fresh, ttl).is_none());

        let expired = scanned_at + SMART_SCAN_CACHE_TTL + Duration::from_secs(1);
        assert!(cache.ids_for(&smart_filter(), expired, ttl).is_none());
    }

    #[test]
    fn long_not_heard_deduplicates_albums_without_changing_first_seen_order() {
        let items = ["album-b", "album-a", "album-b", "album-c"]
            .into_iter()
            .map(|album_id| BaseItem {
                album_id: Some(album_id.into()),
                ..BaseItem::default()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            unique_album_ids(items),
            vec![
                "album-b".to_string(),
                "album-a".to_string(),
                "album-c".to_string()
            ]
        );
    }

    #[test]
    fn collection_playback_keeps_album_then_track_order() {
        assert_eq!(
            flatten_album_tracks(vec![vec!["a1", "a2"], vec!["b1"], vec!["c1", "c2"]]),
            vec!["a1", "a2", "b1", "c1", "c2"]
        );
    }
}

#[cfg(test)]
mod album_grid_tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn played_track(id: &str, album: Option<&str>, plays: i32, last: Option<&str>) -> BaseItem {
        BaseItem {
            id: id.into(),
            album_id: album.map(str::to_string),
            user_data: Some(UserDataWire {
                play_count: plays,
                last_played_date: last.map(str::to_string),
                ..UserDataWire::default()
            }),
            ..BaseItem::default()
        }
    }

    fn stats(entries: &[(&str, i64, Option<&str>)]) -> HashMap<String, AlbumPlayStats> {
        entries
            .iter()
            .map(|(id, play_count, last)| {
                (
                    (*id).to_string(),
                    AlbumPlayStats {
                        play_count: *play_count,
                        last_played: last.map(str::to_string),
                    },
                )
            })
            .collect()
    }

    fn query(sort: AlbumSort, played: PlayedFilter) -> AlbumGridQuery {
        AlbumGridQuery {
            user_id: "user".into(),
            genre_id: None,
            sort,
            favorites_only: false,
            played,
        }
    }

    #[test]
    fn sort_keys_parse_and_every_server_sort_key_gets_its_own_order() {
        assert_eq!(AlbumSort::parse("playCount"), AlbumSort::PlayCount);
        assert_eq!(AlbumSort::parse("datePlayed"), AlbumSort::DatePlayed);
        assert_eq!(AlbumSort::parse("dateAdded"), AlbumSort::DateAdded);
        assert_eq!(AlbumSort::parse("year"), AlbumSort::Year);
        assert_eq!(AlbumSort::parse("random"), AlbumSort::Random);
        assert_eq!(AlbumSort::parse("bogus"), AlbumSort::Name);
        for sort in [
            AlbumSort::Name,
            AlbumSort::DateAdded,
            AlbumSort::Year,
            AlbumSort::Random,
            AlbumSort::PlayCount,
            AlbumSort::DatePlayed,
        ] {
            let (sort_by, sort_order) = sort.server_order();
            assert_eq!(
                sort_by.split(',').count(),
                sort_order.split(',').count(),
                "{sort:?}"
            );
        }
        assert_eq!(
            AlbumSort::Year.server_order(),
            ("ProductionYear,SortName", "Descending,Ascending")
        );
        // Play-based sorts are ordered locally on top of name order.
        assert_eq!(
            AlbumSort::PlayCount.server_order(),
            ("SortName", "Ascending")
        );
    }

    #[test]
    fn only_grids_without_play_data_page_on_the_server() {
        assert!(query(AlbumSort::Name, PlayedFilter::All).is_server_side());
        assert!(AlbumGridQuery {
            genre_id: Some("rock".into()),
            favorites_only: true,
            ..query(AlbumSort::Year, PlayedFilter::All)
        }
        .is_server_side());
        for local in [
            query(AlbumSort::PlayCount, PlayedFilter::All),
            query(AlbumSort::DatePlayed, PlayedFilter::All),
            query(AlbumSort::Name, PlayedFilter::Played),
            query(AlbumSort::Random, PlayedFilter::Unplayed),
        ] {
            assert!(!local.is_server_side(), "{local:?}");
        }
    }

    #[test]
    fn server_params_carry_genre_and_favorites_but_no_album_played_filter() {
        let grid = AlbumGridQuery {
            genre_id: Some("rock".into()),
            favorites_only: true,
            ..query(AlbumSort::DateAdded, PlayedFilter::Played)
        };
        let params = album_grid_params(&grid, grid.sort);
        let get = |key: &str| {
            params
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("genreIds"), Some("rock"));
        assert_eq!(get("filters"), Some("IsFavorite"));
        assert_eq!(get("sortBy"), Some("DateCreated,SortName"));
        assert_eq!(get("sortOrder"), Some("Descending,Ascending"));

        let plain = album_grid_params(
            &query(AlbumSort::Name, PlayedFilter::Unplayed),
            AlbumSort::Name,
        );
        assert!(plain.iter().all(|(name, _)| *name != "filters"));
    }

    #[test]
    fn played_tracks_fold_into_per_album_stats() {
        let mut aggregate = AlbumPlayAggregate::default();
        aggregate.add([
            played_track("t1", Some("a"), 3, Some("2024-01-02T10:00:00.0000000Z")),
            played_track("t2", Some("a"), 2, Some("2024-03-01T08:00:00.0000000Z")),
            // Marked played by hand: the album counts as played, without plays.
            played_track("t3", Some("b"), 0, None),
            // No album to credit.
            played_track("t4", None, 9, Some("2025-01-01T00:00:00.0000000Z")),
        ]);
        aggregate.add([
            // A later scan page repeating a track does not count it twice.
            played_track("t2", Some("a"), 2, Some("2024-03-01T08:00:00.0000000Z")),
            // Reported played without user data: still a played album.
            BaseItem {
                id: "t5".into(),
                album_id: Some("c".into()),
                ..BaseItem::default()
            },
        ]);
        assert_eq!(
            aggregate.albums,
            stats(&[
                ("a", 5, Some("2024-03-01T08:00:00.0000000Z")),
                ("b", 0, None),
                ("c", 0, None),
            ])
        );
    }

    #[test]
    fn played_filter_splits_albums_by_their_tracks() {
        let all = ids(&["a", "b", "c", "d"]);
        // "x" was played but did not pass the server-side filters.
        let played = stats(&[("b", 1, None), ("d", 4, None), ("x", 2, None)]);
        assert_eq!(
            select_grid_albums(all.clone(), &played, PlayedFilter::Played, AlbumSort::Name),
            ids(&["b", "d"])
        );
        assert_eq!(
            select_grid_albums(
                all.clone(),
                &played,
                PlayedFilter::Unplayed,
                AlbumSort::Name
            ),
            ids(&["a", "c"])
        );
        assert_eq!(
            select_grid_albums(all.clone(), &played, PlayedFilter::All, AlbumSort::Year),
            all
        );
    }

    #[test]
    fn play_count_sort_ranks_most_played_first_ties_by_name_unplayed_last() {
        // Server (name) order.
        let names = ids(&["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]);
        let played = stats(&[
            ("bravo", 3, None),
            ("charlie", 7, None),
            ("echo", 3, None),
            ("foxtrot", 0, None),
        ]);
        assert_eq!(
            select_grid_albums(
                names.clone(),
                &played,
                PlayedFilter::All,
                AlbumSort::PlayCount
            ),
            ids(&["charlie", "bravo", "echo", "alpha", "delta", "foxtrot"])
        );
        // The played filter and the sort combine.
        assert_eq!(
            select_grid_albums(names, &played, PlayedFilter::Played, AlbumSort::PlayCount),
            ids(&["charlie", "bravo", "echo", "foxtrot"])
        );
        // An album the stats do not know at all (not scanned yet) ranks last.
        assert_eq!(
            select_grid_albums(
                ids(&["new", "charlie"]),
                &played,
                PlayedFilter::All,
                AlbumSort::PlayCount
            ),
            ids(&["charlie", "new"])
        );
    }

    #[test]
    fn date_played_sort_orders_by_the_latest_track_play() {
        let names = ids(&["a", "b", "c", "d", "e"]);
        let played = stats(&[
            ("a", 1, Some("2024-05-01T00:00:00.0000000Z")),
            // Plays but no date: ranks with the never-played albums.
            ("b", 9, None),
            ("c", 1, Some("2025-01-01T00:00:00.0000000Z")),
            ("d", 1, Some("2024-05-01T00:00:00.0000000Z")),
        ]);
        assert_eq!(
            select_grid_albums(names, &played, PlayedFilter::All, AlbumSort::DatePlayed),
            ids(&["c", "a", "d", "b", "e"])
        );
    }

    #[test]
    fn ranking_direction_flips_only_the_ranked_albums() {
        let order = ids(&["a", "b", "c", "d"]);
        let key = |id: &str| match id {
            "a" | "d" => Some(2),
            "c" => Some(1),
            _ => None,
        };
        assert_eq!(
            rank_album_ids(order.clone(), false, key),
            ids(&["c", "a", "d", "b"])
        );
        assert_eq!(rank_album_ids(order, true, key), ids(&["a", "d", "c", "b"]));
    }

    #[test]
    fn page_slices_stay_within_bounds() {
        let all = ids(&["a", "b", "c", "d", "e"]);
        assert_eq!(page_of_ids(&all, 0, 2), ids(&["a", "b"]));
        assert_eq!(page_of_ids(&all, 4, 2), ids(&["e"]));
        assert_eq!(page_of_ids(&all, 0, u32::MAX), all);
        assert!(page_of_ids(&all, 5, 2).is_empty());
        assert!(page_of_ids(&all, 1, 0).is_empty());
        assert!(page_of_ids(&all, u32::MAX, u32::MAX).is_empty());
        assert!(page_of_ids(&[], 0, 10).is_empty());
    }

    #[test]
    fn grid_scan_cache_serves_only_the_same_query_within_its_ttl() {
        let scanned_at = Instant::now();
        let key = query(AlbumSort::PlayCount, PlayedFilter::Played);
        let cache = ScanCache {
            key: key.clone(),
            scanned_at,
            ids: Arc::new(ids(&["a"])),
        };
        let ttl = ALBUM_SCAN_CACHE_TTL;
        let fresh = scanned_at + Duration::from_secs(5);
        assert!(cache.ids_for(&key, fresh, ttl).is_some());
        for other in [
            AlbumGridQuery {
                played: PlayedFilter::Unplayed,
                ..key.clone()
            },
            AlbumGridQuery {
                sort: AlbumSort::DatePlayed,
                ..key.clone()
            },
            AlbumGridQuery {
                favorites_only: true,
                ..key.clone()
            },
            AlbumGridQuery {
                genre_id: Some("rock".into()),
                ..key.clone()
            },
            AlbumGridQuery {
                user_id: "someone-else".into(),
                ..key.clone()
            },
        ] {
            assert!(cache.ids_for(&other, fresh, ttl).is_none(), "{other:?}");
        }
        let expired = scanned_at + ALBUM_SCAN_CACHE_TTL + Duration::from_secs(1);
        assert!(cache.ids_for(&key, expired, ttl).is_none());
    }
}

#[cfg(test)]
mod token_probe_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A local HTTP server that answers every request with `status` after a
    /// short delay (one connection at a time) and counts the requests.
    fn probe_server(status: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe server");
        let url = format!("http://{}", listener.local_addr().expect("server address"));
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                // Long enough that concurrent callers really queue on the probe.
                std::thread::sleep(Duration::from_millis(100));
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (url, hits)
    }

    fn session(url: &str) -> JellyfinClient {
        JellyfinClient::new(url, "test-device", Vec::new())
            .expect("client")
            .with_session("session".into(), "user".into())
    }

    #[tokio::test]
    async fn a_burst_of_401s_shares_one_token_probe() {
        let (url, hits) = probe_server("200 OK");
        let client = session(&url);
        let at = Instant::now();
        let results = tokio::join!(
            client.validate_after(at),
            client.validate_after(at),
            client.validate_after(at),
            client.validate_after(at),
        );
        let valid = SessionValidity::Valid;
        assert_eq!(results, (valid, valid, valid, valid));
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // A 401 newer than the last probe is not answered by it.
        assert_eq!(client.validate_after(Instant::now()).await, valid);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn the_probe_tells_a_rejected_token_from_server_trouble() {
        let (url, _) = probe_server("401 Unauthorized");
        assert_eq!(
            session(&url).validate_after(Instant::now()).await,
            SessionValidity::Invalid
        );
        let (url, _) = probe_server("503 Service Unavailable");
        assert_eq!(
            session(&url).validate_after(Instant::now()).await,
            SessionValidity::Unreachable
        );
    }
}

/// Live smoke test against a real Jellyfin 10.11 server. Ignored by default —
/// run it explicitly with credentials to exercise the Phase 4-6 API flows
/// end-to-end (the risky server-contract part that unit tests can't cover):
///
/// ```text
/// JELLYSIC_TEST_URL=https://jellyfin.example.com \
/// JELLYSIC_TEST_USER=alice JELLYSIC_TEST_PASS=secret \
/// cargo test --lib smoke -- --ignored --nocapture
/// ```
///
/// It creates and deletes a clearly-named temporary playlist and resets the
/// one favorite it toggles, so it leaves the library as it found it (unless an
/// assertion fails mid-run, in which case the "delete me" playlist remains).
#[cfg(test)]
mod smoke {
    use super::*;

    #[tokio::test]
    #[ignore = "needs a live server; set JELLYSIC_TEST_{URL,USER,PASS}"]
    async fn live_flows() {
        let (Ok(url), Ok(user), Ok(pass)) = (
            std::env::var("JELLYSIC_TEST_URL"),
            std::env::var("JELLYSIC_TEST_USER"),
            std::env::var("JELLYSIC_TEST_PASS"),
        ) else {
            eprintln!("skipping: set JELLYSIC_TEST_URL / _USER / _PASS to run");
            return;
        };

        let mut client =
            JellyfinClient::new(url.trim_end_matches('/'), "jellysic-smoke", Vec::new())
                .expect("client");
        client.authenticate(&user, &pass).await.expect("login");
        eprintln!("✓ authenticated");

        // --- browse / discovery ---
        let albums = client
            .albums(0, 20, None, "name", false, "all")
            .await
            .expect("albums");
        eprintln!("✓ albums: {} of {}", albums.items.len(), albums.total);
        let genres = client.genres().await.expect("genres");
        eprintln!("✓ genres: {}", genres.len());
        if let Some(g) = genres.first() {
            let ga = client
                .albums(0, 5, Some(&g.id), "name", false, "all")
                .await
                .expect("genre-filtered albums");
            eprintln!("✓ genre '{}' filter: {} albums", g.name, ga.items.len());
        }
        let idx_alb = client
            .items_before_letter("albums", "M", None)
            .await
            .expect("letter_index albums");
        let idx_art = client
            .items_before_letter("artists", "M", None)
            .await
            .expect("letter_index artists");
        eprintln!("✓ letter_index 'M': albums={idx_alb}, artists={idx_art}");
        let artists = client.artists(0, 20).await.expect("artists");
        eprintln!("✓ artists: {}", artists.items.len());
        let songs = client.songs(0, 20).await.expect("songs");
        eprintln!("✓ songs: {} of {}", songs.items.len(), songs.total);

        // --- home rows (these query tracks and map to albums) ---
        for row in ["DateCreated", "DatePlayed", "PlayCount"] {
            let r = client.home_albums(row, 12).await.expect("home_albums");
            eprintln!("✓ home '{row}': {} albums", r.len());
        }

        // --- album grid: played state and play data come from tracks ---
        let grid_all = client
            .albums(0, 1, None, "name", false, "all")
            .await
            .expect("albums (all)");
        let grid_played = client
            .albums(0, 20, None, "name", false, "played")
            .await
            .expect("albums (played)");
        let grid_unplayed = client
            .albums(0, 20, None, "name", false, "unplayed")
            .await
            .expect("albums (unplayed)");
        assert_eq!(
            grid_played.total + grid_unplayed.total,
            grid_all.total,
            "played + unplayed albums = all albums"
        );
        let most_played = client
            .albums(0, 10, None, "playCount", false, "all")
            .await
            .expect("albums (playCount)");
        let most_played_next = client
            .albums(10, 10, None, "playCount", false, "all")
            .await
            .expect("albums (playCount, cached page)");
        assert_eq!(most_played.total, grid_all.total, "a sort keeps the total");
        assert!(
            most_played_next
                .items
                .iter()
                .all(|next| most_played.items.iter().all(|first| first.id != next.id)),
            "the cached page continues without repeats"
        );
        eprintln!(
            "✓ album grid: played={}, unplayed={}, all={}, most played first page={}",
            grid_played.total,
            grid_unplayed.total,
            grid_all.total,
            most_played.items.len()
        );

        // --- favorites, paged per section ---
        let fav_tracks = client.favorite_tracks(0, 5).await.expect("favorite_tracks");
        let fav_albums = client.favorite_albums(0, 5).await.expect("favorite_albums");
        let fav_artists = client
            .favorite_artists(0, 5)
            .await
            .expect("favorite_artists");
        eprintln!(
            "✓ favorites: tracks {} of {}, albums {} of {}, artists {} of {}",
            fav_tracks.items.len(),
            fav_tracks.total,
            fav_albums.items.len(),
            fav_albums.total,
            fav_artists.items.len(),
            fav_artists.total
        );
        if fav_artists.total > 5 {
            let next = client
                .favorite_artists(5, 5)
                .await
                .expect("favorite_artists (page 2)");
            assert!(
                next.items
                    .iter()
                    .all(|a| fav_artists.items.iter().all(|b| b.id != a.id)),
                "favorite artist pages do not repeat"
            );
        }

        // --- Phase 11 Discover / smart views / read-only collections ---
        let random = client
            .discover_random_albums(0, 12)
            .await
            .expect("discover_random_albums");
        let seeds = client
            .discover_mix_seeds(0, 12)
            .await
            .expect("discover_mix_seeds");
        let years = client.music_years().await.expect("music_years");
        let long_not_heard = client
            .long_not_heard_albums(0, 12)
            .await
            .expect("long_not_heard_albums");
        let smart = client
            .smart_tracks_page(&SmartFilter::default(), 0, 20)
            .await
            .expect("smart_tracks_page");
        eprintln!(
            "✓ discover: random={}, seeds={}, years={}, long_not_heard={}, smart={} of {}",
            random.items.len(),
            seeds.items.len(),
            years.len(),
            long_not_heard.items.len(),
            smart.items.len(),
            smart.total
        );

        // Seed picker + decade drill-down (contracts behind the Discover artist
        // row and the play_discover_decade path).
        let artist_seed = client
            .discover_artist_seed()
            .await
            .expect("discover_artist_seed");
        eprintln!(
            "✓ discover_artist_seed: {}",
            artist_seed
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or("<none>")
        );
        if let Some(year) = years.iter().copied().max() {
            let decade_start = year - year.rem_euclid(10);
            if decade_start >= 10 {
                let decade = client
                    .decade_albums(decade_start, 0, 12)
                    .await
                    .expect("decade_albums");
                eprintln!(
                    "✓ decade_albums {decade_start}s: {} of {}",
                    decade.items.len(),
                    decade.total
                );
            }
        }

        // Non-trivial smart filter through the un-paged smart_tracks the
        // play_smart_view / materialize_smart_view paths use — verifies the
        // filter → query-param mapping (genre + played), not just the default.
        let constrained = SmartFilter {
            genre_ids: genres
                .first()
                .map(|g| vec![g.id.clone()])
                .unwrap_or_default(),
            played: PlayedFilter::Played,
            ..SmartFilter::default()
        };
        let constrained_tracks = client
            .smart_tracks(&constrained)
            .await
            .expect("smart_tracks (genre + played)");
        eprintln!(
            "✓ smart_tracks (genre + played): {} tracks",
            constrained_tracks.len()
        );

        // A filter the server evaluates completely is paged by the server. That
        // is only correct if the server's page is exactly the scan's slice —
        // same members, same order, same total.
        let paged = client
            .smart_tracks_page(&constrained, 0, 20)
            .await
            .expect("smart_tracks_page (server-side)");
        assert_eq!(
            paged.total,
            constrained_tracks.len() as i64,
            "server total vs scan"
        );
        assert_eq!(
            paged
                .items
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            constrained_tracks
                .iter()
                .take(20)
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            "server page vs scan order"
        );
        // A local predicate goes through the scan; the second page comes from
        // the cached ids and must continue exactly where the first ended.
        let local = SmartFilter {
            min_play_count: Some(1),
            ..SmartFilter::default()
        };
        let all_local = client
            .smart_tracks(&local)
            .await
            .expect("smart_tracks (minPlayCount)");
        let first = client
            .smart_tracks_page(&local, 0, 10)
            .await
            .expect("smart_tracks_page (scan)");
        let second = client
            .smart_tracks_page(&local, 10, 10)
            .await
            .expect("smart_tracks_page (cached ids)");
        assert_eq!(first.total, all_local.len() as i64, "scan total");
        assert_eq!(
            first
                .items
                .iter()
                .chain(&second.items)
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            all_local
                .iter()
                .take(20)
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            "scan pages vs full scan"
        );
        eprintln!(
            "✓ smart paging: server page matches scan ({} of {}), cached scan pages match ({} of {})",
            paged.items.len(),
            paged.total,
            first.items.len() + second.items.len(),
            first.total
        );

        let collections = client.collections(0, 20).await.expect("collections");
        if let Some(collection) = collections.items.first() {
            let detail = client.collection(&collection.id).await.expect("collection");
            let albums = client
                .collection_albums(&collection.id)
                .await
                .expect("collection_albums");
            let tracks = client
                .collection_tracks(&collection.id)
                .await
                .expect("collection_tracks");
            eprintln!(
                "✓ collection '{}': albums={}, tracks={}",
                detail.name,
                albums.len(),
                tracks.len()
            );
        }

        // --- stats + library signature ---
        let rp = client
            .recently_played_tracks(20)
            .await
            .expect("recently_played");
        let mp = client.most_played_tracks(20).await.expect("most_played");
        eprintln!("✓ stats: recently={}, most_played={}", rp.len(), mp.len());
        let sig = client.library_signature().await.expect("library_signature");
        eprintln!(
            "✓ library_signature: count={}, newest='{}'",
            sig.count, sig.newest
        );

        // --- artist detail ---
        if let Some(a) = artists.items.first() {
            let artist = client.artist(&a.id).await.expect("artist");
            let al = client.artist_albums(&a.id).await.expect("artist_albums");
            let ap = client
                .artist_appears_on(&a.id)
                .await
                .expect("artist_appears_on");
            let ts = client
                .artist_top_songs(&a.id, 5)
                .await
                .expect("artist_top_songs");
            let sim = client
                .similar_artists(&a.id, 12)
                .await
                .expect("similar_artists");
            eprintln!(
                "✓ artist '{}': overview={}, albums={}, appears_on={}, top_songs={}, similar={}",
                artist.name,
                artist.overview.is_some(),
                al.len(),
                ap.len(),
                ts.len(),
                sim.len()
            );
        }

        // --- album detail, instant mix, favorite, playlist CRUD ---
        if let Some(al) = albums.items.first() {
            let album = client.album(&al.id).await.expect("album");
            let tracks = client.album_tracks(&al.id).await.expect("album_tracks");
            let mix = client.instant_mix(&al.id, 10).await.expect("instant_mix");
            eprintln!(
                "✓ album '{}': {} tracks, instant_mix={}",
                album.name,
                tracks.len(),
                mix.len()
            );

            if let Some(t) = tracks.first() {
                client.set_favorite(&t.id, true).await.expect("favorite");
                client.set_favorite(&t.id, false).await.expect("unfavorite");
                eprintln!("✓ favorite toggle round-trip");
            }

            let seed: Vec<String> = tracks.iter().take(2).map(|t| t.id.clone()).collect();
            if !seed.is_empty() {
                let pid = client
                    .create_playlist("Jellysic smoke test — delete me", &seed[..1])
                    .await
                    .expect("create_playlist");
                eprintln!("✓ created playlist {pid}");
                if seed.len() >= 2 {
                    client
                        .playlist_add_chunk(&pid, &seed[1..2])
                        .await
                        .expect("playlist_add");
                }
                let before = client.playlist_tracks(&pid).await.expect("playlist_tracks");
                let n_before = before.len();

                // The key contract to verify: a Name-only rename must NOT drop
                // the tracks (older UpdatePlaylistDto implementations did).
                client
                    .rename_playlist(&pid, "Jellysic smoke test — renamed")
                    .await
                    .expect("rename");
                let after = client
                    .playlist_tracks(&pid)
                    .await
                    .expect("playlist_tracks after rename");
                assert_eq!(
                    after.len(),
                    n_before,
                    "rename must preserve tracks (had {n_before}, now {})",
                    after.len()
                );
                let renamed = client.playlist(&pid).await.expect("playlist after rename");
                assert_eq!(
                    renamed.name, "Jellysic smoke test — renamed",
                    "name updated"
                );
                eprintln!("✓ rename preserved {n_before} tracks");

                if after.len() >= 2 {
                    client
                        .playlist_move(&pid, &after[0].entry_id, 1)
                        .await
                        .expect("playlist_move");
                }
                if let Some(first) = after.first() {
                    client
                        .playlist_remove(&pid, std::slice::from_ref(&first.entry_id))
                        .await
                        .expect("playlist_remove");
                }
                client.delete_playlist(&pid).await.expect("delete_playlist");
                eprintln!("✓ playlist add/rename/move/remove/delete");
            }
        }

        eprintln!("\nAll smoke flows passed.");
    }
}
