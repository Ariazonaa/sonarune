pub mod types;

#[cfg(test)]
mod client_tests;

use crate::error::{AppError, AppResult};
use serde::de::DeserializeOwned;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use types::*;

pub const CLIENT_NAME: &str = "Sonarune";
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Sent with every request. reqwest sends none of its own, and a proxy in
/// front of the server may refuse a request without one: Cloudflare's Browser
/// Integrity Check (on by default) answers it with a 403 challenge, which the
/// session probe would then read as a rejected token.
pub const USER_AGENT: &str = concat!("Sonarune/", env!("CARGO_PKG_VERSION"));

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

/// A Jellyfin item id: a GUID, as the server writes it (32 hex digits) or with
/// its dashes.
fn is_item_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    match bytes.len() {
        32 => bytes.iter().all(u8::is_ascii_hexdigit),
        36 => bytes.iter().enumerate().all(|(at, byte)| {
            if matches!(at, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }),
        _ => false,
    }
}

/// An item id in one spelling, whatever the server or a stored value used.
fn item_id_key(id: &str) -> String {
    id.chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// How many of the entries a playlist holds (`stored`) are not among the ones
/// listed for the user (`listed`): the entries hidden from them. Visibility
/// goes by item, so it is enough to compare which items are listed.
fn hidden_entries(stored: &[String], listed: &[String]) -> usize {
    let listed = listed
        .iter()
        .map(|id| item_id_key(id))
        .collect::<HashSet<_>>();
    stored
        .iter()
        .filter(|id| !listed.contains(&item_id_key(id)))
        .count()
}

/// Most of a non-2xx body read for the error message — a proxy's error page
/// can be arbitrarily large and is never worth buffering whole.
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;
/// Error messages end up in the UI banner and the log; keep them short.
const MAX_ERROR_MESSAGE_CHARS: usize = 300;

/// A bounded, banner-sized message from a failed response's body.
async fn error_message(mut response: reqwest::Response) -> String {
    // A proxy's redirect usually has no body at all, and "server returned
    // 308: " alone does not say that the server moved.
    if let Some(target) = redirect_target(&response) {
        return format!(
            "the server redirects to {} (not followed)",
            target.origin().ascii_serialization()
        );
    }
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

/// Where a redirect the client did not follow points: `Location`, resolved
/// against the request. Redirects are followed only inside the configured
/// origin (`same_origin_redirects`), so one that reaches a caller leaves it —
/// most often a proxy upgrading `http://` to `https://`.
fn redirect_target(response: &reqwest::Response) -> Option<reqwest::Url> {
    if !response.status().is_redirection() {
        return None;
    }
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    response.url().join(location).ok()
}

/// The server address a redirect of `api_path` (a path below `base_url`) asks
/// the client to use instead — but only for an upgrade to `https://` on the
/// same host: the target with the API path cut off, so a server under a
/// sub-path keeps it. `None` for any other redirect: another host may be a
/// captive portal, and is no address to suggest a password for.
fn upgraded_base_url(base_url: &str, api_path: &str, target: &reqwest::Url) -> Option<String> {
    let base = reqwest::Url::parse(base_url).ok()?;
    if target.scheme() != "https" || target.host_str() != base.host_str() {
        return None;
    }
    let path = target
        .path()
        .strip_suffix(api_path)
        .unwrap_or(base.path())
        .trim_end_matches('/');
    Some(format!("{}{path}", target.origin().ascii_serialization()))
}

/// Ceiling for a JSON answer. The longest real one is an unpaged playlist
/// (`playlist_tracks`, about 2 KB per entry): the app itself saves smart views
/// of up to [`SMART_SCAN_MAX_ITEMS`] tracks as playlists — some 200 MB to read
/// back — and other clients can make longer ones. A scan page of 1000 items is
/// a few MB. The ceiling clears both and still keeps a broken or hostile
/// server from making the client buffer gigabytes (see [`MAX_IMAGE_BYTES`]).
const MAX_JSON_BYTES: u64 = 512 * 1024 * 1024;

// The longest playlist the app makes must stay readable, with twice the room
// an entry takes.
const _: () = assert!(SMART_SCAN_MAX_ITEMS as u64 * 4 * 1024 <= MAX_JSON_BYTES);

/// A JSON body, read with [`MAX_JSON_BYTES`] as the limit.
async fn read_json<T: DeserializeOwned>(response: reqwest::Response) -> AppResult<T> {
    read_json_capped(response, MAX_JSON_BYTES).await
}

async fn read_json_capped<T: DeserializeOwned>(
    response: reqwest::Response,
    limit: u64,
) -> AppResult<T> {
    let body = read_capped(response, limit, "server response").await?;
    serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(format!("unreadable server response: {e}")))
}

/// A by-id lookup that came back empty: the item is gone, or this user may
/// not see it. Jellyfin answers such an `/Items?ids=` query with an empty 200,
/// never a 404 — reported as one, so a page can tell a deleted item from a
/// request that failed.
fn not_found(kind: &str, id: &str) -> AppError {
    AppError::Server {
        status: 404,
        message: format!("{kind} {id} not found"),
    }
}

/// A detail page's item and its contents, fetched side by side. When both
/// fail, the item's error wins: for an item deleted on the server, the lookup
/// by id says 404 while the contents query fails with whatever an unknown
/// parent gets (a 400 for an album), and the page drops its cached copy only
/// on that 404. `try_join!` would report whichever happened to come back first.
pub async fn join_detail<A, B>(
    item: impl std::future::Future<Output = AppResult<A>>,
    contents: impl std::future::Future<Output = AppResult<B>>,
) -> AppResult<(A, B)> {
    let (item, contents) = tokio::join!(item, contents);
    Ok((item?, contents?))
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
/// copied tracks keep their order in the freshly fetched source playlist. An
/// entry id the source lists more than once — on Jellyfin 10.11 that is a
/// track in the playlist twice, whose entries both carry the track's id —
/// resolves once, at its first entry.
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

/// The index Jellyfin's playlist move needs for an entry to land at
/// `audio_index` among the playlist's audio entries — the list
/// `playlist_tracks` returns and the playlist page reorders.
///
/// `entries` is the playlist as Jellyfin lists it to the user, in order: each
/// entry's id and whether it is audio. Jellyfin counts its move index over
/// all of them (remove-then-insert, like the queue), so every non-audio entry
/// `playlist_tracks` leaves out shifts it. The entry goes right below the
/// audio entry it was dropped under, or right above the one it was dropped
/// over, which keeps the hidden entries where they were among the rest.
/// None when the entry is not in the playlist; the first match counts, as it
/// does on the server.
fn playlist_move_index(
    entries: &[(&str, bool)],
    entry_id: &str,
    audio_index: usize,
) -> Option<usize> {
    use std::cmp::Ordering;

    let from = entries.iter().position(|(id, _)| *id == entry_id)?;
    let from_audio = entries[..from].iter().filter(|(_, audio)| *audio).count();
    // Where the other audio entries sit once the moved one is taken out: the
    // list Jellyfin inserts it back into.
    let rest: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(index, (_, audio))| *audio && *index != from)
        .map(|(index, _)| if index > from { index - 1 } else { index })
        .collect();
    let target = audio_index.min(rest.len());
    Some(match target.cmp(&from_audio) {
        Ordering::Greater => rest[target - 1] + 1,
        Ordering::Less => rest[target],
        // Its audio position does not change; the server takes its own
        // index as "no move".
        Ordering::Equal => from,
    })
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

/// Tickets for one kind of paged browsing scan (smart views, the album grid).
///
/// A running command cannot be cancelled from the frontend: a view that was
/// replaced (a filter edit, another sort) only drops the old answer, while its
/// scan walks on through the whole library and, finishing last, would put the
/// old view's ids into the single-slot cache. So every such scan takes a
/// ticket. A newer one supersedes it: the old scan stops before its next page,
/// and only the scan holding the newest ticket writes the cache.
#[derive(Debug, Default)]
struct ScanTickets(AtomicU64);

impl ScanTickets {
    fn take(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_current(&self, ticket: u64) -> bool {
        self.0.load(Ordering::SeqCst) == ticket
    }
}

/// What a superseded scan stops with. Nobody gets to see it: the view that
/// asked has already moved on and drops the answer.
fn superseded() -> AppError {
    AppError::Other("superseded by a newer query".into())
}

/// Keep `ids` as the scan cache for `key`, unless a newer scan of the same
/// kind started in the meantime — its own result belongs there.
fn cache_scan<K>(
    slot: &Mutex<Option<ScanCache<K>>>,
    tickets: &ScanTickets,
    ticket: u64,
    key: K,
    ids: Arc<Vec<String>>,
) {
    let mut slot = slot.lock().unwrap();
    if tickets.is_current(ticket) {
        *slot = Some(ScanCache {
            key,
            scanned_at: Instant::now(),
            ids,
        });
    }
}

/// Page size of the album grid's scans (played tracks, matching album ids).
/// Both requests are ids plus user data only, so a large page stays cheap.
const ALBUM_SCAN_BATCH: u32 = 1000;

/// How long later pages of a locally evaluated album grid reuse its scan. As
/// with smart views, the first page always rescans.
const ALBUM_SCAN_CACHE_TTL: Duration = Duration::from_secs(60);

/// How long later pages of "long not heard", and the Discover row, reuse the
/// last ranking (the "see all" page's first page rescans). A play moves its
/// album from wherever it was to the very end, so a rescan between two pages
/// would shift the page boundary by one album per play — tolerable, but not
/// on every page.
const LONG_NOT_HEARD_CACHE_TTL: Duration = Duration::from_secs(300);

/// Page size and reach of a home row's walk through the played tracks.
const HOME_TRACK_BATCH: u32 = 200;
const HOME_TRACK_MAX: u32 = 2_000;

/// A box set's music-album count, and its member count (`ChildCount`) when it
/// was counted.
#[derive(Debug, Clone, Copy)]
struct CollectionCount {
    members: i32,
    albums: i64,
    counted_at: Instant,
}

/// How long a box set's album count is trusted while its member count stays
/// the same — a swap of one member for another keeps the count.
const COLLECTION_COUNT_TTL: Duration = Duration::from_secs(600);

/// Box sets counted at the same time.
const COLLECTION_COUNT_CONCURRENCY: usize = 6;

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

/// What a locally evaluated grid's scan is cached under: its query and, for a
/// random grid, the seed of the shuffle it pages through.
type AlbumGridKey = (AlbumGridQuery, Option<u64>);

impl AlbumGridQuery {
    /// Whether the server can filter and order this grid itself, and so page
    /// it. Played state and play data live on tracks, not albums (an album's
    /// `IsPlayed`/`IsUnplayed` filter reads the album's own, never-written
    /// user data): those need the local scan. So does a random order:
    /// `sortBy=Random` reshuffles on every request, so the server's pages
    /// would overlap, and the grid stopped at the first page that brought
    /// nothing new, with some albums never shown.
    fn is_server_side(&self) -> bool {
        !self.sort.is_play_based()
            && self.sort != AlbumSort::Random
            && self.played == PlayedFilter::All
    }

    /// Whether evaluating this grid locally needs the per-album play stats —
    /// only a play-based sort or the played filter reads them.
    fn needs_play_stats(&self) -> bool {
        self.sort.is_play_based() || self.played != PlayedFilter::All
    }
}

/// Fisher-Yates driven by SplitMix64: the same seed always lays the same items
/// out the same way. That is what lets a random album grid continue one
/// shuffle across a rescan instead of drawing a new one mid-list.
fn shuffle_seeded<T>(items: &mut [T], seed: u64) {
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    for upper in (1..items.len()).rev() {
        let other = (next() % (upper as u64 + 1)) as usize;
        items.swap(upper, other);
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

/// Played albums, the one heard longest ago first. An album was last heard
/// when any of its tracks last played, so it ranks by its latest track play;
/// albums whose plays carry no date at all cannot be placed and come last.
/// Ties keep id order, so two scans of the same plays rank the same way.
fn least_recently_played(stats: &HashMap<String, AlbumPlayStats>) -> Vec<String> {
    let mut ids = stats.keys().cloned().collect::<Vec<_>>();
    ids.sort_unstable();
    rank_album_ids(ids, false, |id| {
        stats.get(id).and_then(|s| s.last_played.as_deref())
    })
}

/// Append the albums of `items` that `albums` does not hold yet, in order,
/// until it holds `limit`. Returns whether it is full.
fn take_new_albums(albums: &mut Vec<String>, items: Vec<BaseItem>, limit: usize) -> bool {
    for album_id in items.into_iter().filter_map(|item| item.album_id) {
        if albums.len() >= limit {
            break;
        }
        if !album_id.is_empty() && !albums.contains(&album_id) {
            albums.push(album_id);
        }
    }
    albums.len() >= limit
}

/// The music libraries among a user's views (`/UserViews`).
fn music_libraries(views: Vec<BaseItem>) -> Vec<String> {
    views
        .into_iter()
        .filter(|view| {
            view.collection_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("music"))
        })
        .map(|view| view.id)
        .filter(|id| !id.is_empty())
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

/// The part of a smart filter checked on each scanned track. Genres are not
/// among it: every scan already sends `genreIds`, and the server matches those
/// by name, while an id comparison here can fail for the right track — the
/// `Genre` item `/Genres` lists for a name (which saved views may still carry)
/// and the music genre a track lists under the same name have different ids,
/// so on Jellyfin 10.11 it rejected every track the server had found.
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

/// The matches of a smart-view scan, each track once. The scan pages by
/// offset over "newest first", so a track added while it runs shifts every
/// later page by one, and the last track of one page comes back as the first
/// of the next (`AlbumPlayAggregate` guards the album grid's scan alike).
#[derive(Debug, Default)]
struct SmartMatches {
    seen: HashSet<String>,
    items: Vec<BaseItem>,
}

impl SmartMatches {
    fn add(&mut self, items: impl IntoIterator<Item = BaseItem>, filter: &SmartFilter) {
        for item in items {
            if !smart_item_matches(&item, filter) {
                continue;
            }
            if !item.id.is_empty() && !self.seen.insert(item.id.clone()) {
                continue;
            }
            self.items.push(item);
        }
    }
}

fn flatten_album_tracks<T>(albums: impl IntoIterator<Item = Vec<T>>) -> Vec<T> {
    albums.into_iter().flatten().collect()
}

/// Album track requests [`JellyfinClient::tracks_for_albums`] keeps in flight.
const ALBUM_TRACKS_IN_FLIGHT: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionValidity {
    Valid,
    /// Token rejected — re-authentication needed.
    Invalid,
    /// Server not reachable or unhealthy; token may well still be good.
    Unreachable,
    /// The handshake failed on a certificate this client does not trust —
    /// the fingerprint it presented. A pinned server that renewed its
    /// certificate looks like this, and so does anything on the network that
    /// presents its own (a captive portal, an interception): the user has to
    /// look at it, a retry alone never gets through.
    Untrusted(String),
    /// The server redirects to `https://` on the same host: the address to
    /// sign in with from now on. The stored one will never work again.
    Moved(String),
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
    /// Fingerprint the most recent handshake presented (set by the TLS verifier,
    /// cleared when a session probe starts — see [`Self::validate`]).
    observed: crate::tls::ObservedFingerprint,
    token: Option<String>,
    user_id: Option<String>,
    /// The Jellyfin id of the server, once the session learned it
    /// (`commands::install_session`); what stored data is filed under
    /// (`identity.rs`). Fixed for the client's life: a session is one server.
    server_id: Option<String>,
    /// Last full smart-view scan, reused by that view's later pages. Per client,
    /// so it never outlives the session (server and user) it was scanned for.
    smart_scan_cache: Mutex<Option<ScanCache<SmartFilter>>>,
    /// Last locally evaluated album-grid scan (play-based sort, played filter
    /// or random order), reused by that grid's later pages. Per client, like
    /// the above.
    album_scan_cache: Mutex<Option<ScanCache<AlbumGridKey>>>,
    /// The random album grid's query and the seed of the shuffle it pages
    /// through (see [`Self::random_grid_seed`]).
    random_grid_shuffle: Mutex<Option<(AlbumGridQuery, u64)>>,
    /// The last "long not heard" ranking, keyed by user, reused by later pages
    /// and the Discover row. The async lock is held through a scan, so calls
    /// that arrive meanwhile wait for its ranking instead of scanning too.
    long_not_heard_cache: tokio::sync::Mutex<Option<ScanCache<String>>>,
    /// Music-album counts of the box sets by id, with the member count each
    /// was taken at (see [`Self::collection_album_counts`]).
    collection_counts: Mutex<HashMap<String, CollectionCount>>,
    /// The last 401 follow-up probe: when it started and what it found. The
    /// async lock makes the probe single-flight (see [`Self::validate_after`]).
    token_probe: tokio::sync::Mutex<Option<(Instant, SessionValidity)>>,
    /// The last automatic re-login that tried to replace this client and
    /// failed without the server refusing the password (a 5xx, a rate limit,
    /// a dropped connection): when it started, and the error.
    failed_relogin: Mutex<Option<(Instant, AppError)>>,
    /// Tickets of the paged scans behind the two caches above.
    smart_scan_tickets: ScanTickets,
    album_scan_tickets: ScanTickets,
}

/// Ceiling for a cover image. Jellyfin's largest rendition is far below this;
/// the point is that a hostile or broken server cannot make the client buffer
/// an unbounded body in memory (the bytes are held three times over: response
/// buffer, cache write, and WebView response).
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Whether `bytes` start like an image the WebView can show: JPEG, PNG,
/// WebP and GIF (what Jellyfin's image encoder writes), BMP, and AVIF/HEIF.
/// Shared by the cover fetch and the cover cache, which must never hold —
/// let alone serve as immutable — the HTML page some portal answered with.
pub(crate) fn looks_like_image(bytes: &[u8]) -> bool {
    let at = |offset: usize, magic: &[u8]| bytes.get(offset..offset + magic.len()) == Some(magic);
    at(0, &[0xFF, 0xD8, 0xFF])
        || at(0, b"\x89PNG\r\n\x1a\n")
        || (at(0, b"RIFF") && at(8, b"WEBP"))
        || at(0, b"GIF87a")
        || at(0, b"GIF89a")
        || at(0, b"BM")
        || (at(4, b"ftyp")
            && [b"avif", b"avis", b"heic", b"heix", b"mif1"]
                .iter()
                .any(|brand| at(8, *brand)))
}

/// How long a sign-out waits for the server to revoke the token.
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long creating a playlist of `tracks` may take. Jellyfin resolves every
/// id before it answers and carries on when the client gives up — a client
/// timing out on a long list left a playlist behind that the UI reported as
/// failed, and saving again made a second one. 10 ms per track on top of the
/// usual 30 s is far beyond what a server needs.
fn create_playlist_timeout(tracks: usize) -> Duration {
    let per_track =
        Duration::from_millis(10).saturating_mul(u32::try_from(tracks).unwrap_or(u32::MAX));
    Duration::from_secs(30).saturating_add(per_track)
}

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

/// A server id as stored data is filed under it: Jellyfin's is a GUID (32
/// hex digits), compared without regard to case or dashes. Anything else —
/// empty, overlong, with spaces or other signs in it — is no id to go by.
fn server_id_from(raw: &str) -> Option<String> {
    let id: String = raw
        .trim()
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (!id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some(id)
}

impl JellyfinClient {
    pub fn new(base_url: &str, device_id: &str, trusted: Vec<String>) -> AppResult<Self> {
        let base = base_url.trim_end_matches('/').to_string();
        let (builder, observed) = crate::tls::apply(
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
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
            server_id: None,
            smart_scan_cache: Mutex::new(None),
            album_scan_cache: Mutex::new(None),
            random_grid_shuffle: Mutex::new(None),
            long_not_heard_cache: tokio::sync::Mutex::new(None),
            collection_counts: Mutex::new(HashMap::new()),
            token_probe: tokio::sync::Mutex::new(None),
            failed_relogin: Mutex::new(None),
            smart_scan_tickets: ScanTickets::default(),
            album_scan_tickets: ScanTickets::default(),
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

    /// The Jellyfin id of the server, when the session knows it.
    pub fn server_id(&self) -> Option<&str> {
        self.server_id.as_deref()
    }

    /// Before the client is shared: which server it talks to.
    pub fn set_server_id(&mut self, id: Option<String>) {
        self.server_id = id;
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
        const PATH: &str = "/Users/AuthenticateByName";
        let url = format!("{}{PATH}", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "Username": username, "Pw": password }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            // Not followed even to https: the password has already gone out
            // on this request. The user signs in at the new address instead.
            let upgraded = redirect_target(&response)
                .and_then(|target| upgraded_base_url(&self.base_url, PATH, &target));
            let message = match (status.as_u16(), upgraded) {
                (401, _) => "wrong username or password".to_string(),
                (_, Some(address)) => {
                    format!("the server redirects to {address} — sign in with that address")
                }
                _ => error_message(response).await,
            };
            return Err(AppError::Server {
                status: status.as_u16(),
                message,
            });
        }
        let auth: AuthenticationResult = read_json(response).await?;
        self.token = Some(auth.access_token);
        self.user_id = Some(auth.user.id.clone());
        Ok(auth.user)
    }

    /// Cheap token validation used on session restore. Distinguishes a dead
    /// token from an unreachable server: offline must not log the user out.
    pub async fn validate(&self) -> SessionValidity {
        const PATH: &str = "/Users/Me";
        let url = format!("{}{PATH}", self.base_url);
        // Only a handshake made for this probe may speak for the certificate.
        // The verifier replaces what it recorded only on its next non-public
        // leaf, so one recorded on a network the device has left since (a
        // captive portal's) would turn every later outage — no DNS, no route,
        // a refused connection — into an untrusted certificate, offered for
        // trust instead of the offline banner (see `tls.rs`).
        *self.observed.lock().unwrap() = None;
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
            Ok(r) => match redirect_target(&r)
                .and_then(|target| upgraded_base_url(&self.base_url, PATH, &target))
            {
                Some(address) => SessionValidity::Moved(address),
                // 5xx, proxies mid-restart, a redirect to another host (a
                // captive portal): server trouble, not an auth problem.
                None => SessionValidity::Unreachable,
            },
            // A rejected certificate says nothing about the token either, but
            // unlike a dead network it does not go away on its own.
            Err(_) => match self.untrusted_fingerprint() {
                Some(fingerprint) => SessionValidity::Untrusted(fingerprint),
                None => SessionValidity::Unreachable,
            },
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
        if let Some((started, validity)) = last.as_ref() {
            if *started >= unauthorized_at {
                return validity.clone();
            }
        }
        let started = Instant::now();
        let validity = self.validate().await;
        *last = Some((started, validity.clone()));
        validity
    }

    /// Remember that an automatic re-login meant to replace this client,
    /// started at `started`, failed with `err`.
    pub fn record_failed_relogin(&self, started: Instant, err: &AppError) {
        *self.failed_relogin.lock().unwrap() = Some((started, err.replay()));
    }

    /// The failure of a re-login that started after a 401 at
    /// `unauthorized_at`, if there was one. It saw the server at least as late
    /// as that 401 did, so the caller shares its answer rather than logging in
    /// once more — what [`Self::validate_after`] does for the probe.
    pub fn failed_relogin_since(&self, unauthorized_at: Instant) -> Option<AppError> {
        self.failed_relogin
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(started, _)| *started >= unauthorized_at)
            .map(|(_, err)| err.replay())
    }

    /// The current user, including policy — used to refresh the delete
    /// permission on session restore (the token path skips authenticate()).
    pub async fn me(&self) -> AppResult<UserDto> {
        self.get_json("/Users/Me", &[]).await
    }

    /// The complete item as raw JSON, for a faithful metadata round-trip:
    /// `GET /Items/{itemId}?userId=` (10.9+), the only fetch that returns
    /// every field — an `/Items` query returns just the fields it names.
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

    /// The server's own id (`/System/Info/Public`), the same under every
    /// address it is reached at. Never logged or exported: it names the
    /// server.
    pub async fn public_server_id(&self) -> AppResult<String> {
        let info: PublicSystemInfo = self.get_json("/System/Info/Public", &[]).await?;
        info.id
            .as_deref()
            .and_then(server_id_from)
            .ok_or_else(|| AppError::Other("the server does not say its id".into()))
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
        read_json(response).await
    }

    /// One page of the album grid.
    ///
    /// The server filters, orders and pages the grid itself unless it needs
    /// play data (a play-based sort or the played filter) or a random order
    /// (see [`AlbumGridQuery::is_server_side`]). Then only a scan knows the
    /// result: the first page scans the albums passing the server-side filters
    /// (and the played tracks, when play data is needed) and evaluates the
    /// rest locally; later pages fetch their slice by id from that scan's ids.
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
        self.album_grid_page(&query, start_index, limit).await
    }

    /// One page of an album grid the server filters, orders and pages itself.
    async fn album_grid_page(
        &self,
        query: &AlbumGridQuery,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<AlbumDto>> {
        let mut params = album_grid_params(query, query.sort);
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
        // The seed is part of the key: a scan that finishes late with another
        // shuffle can never serve this grid's later pages.
        let seed = (query.sort == AlbumSort::Random)
            .then(|| self.random_grid_seed(query, start_index == 0));
        let key = (query.clone(), seed);
        if start_index > 0 {
            let cached = self
                .album_scan_cache
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|cache| cache.ids_for(&key, Instant::now(), ALBUM_SCAN_CACHE_TTL));
            if let Some(ids) = cached {
                return Ok(ids);
            }
        }

        let ticket = self.album_scan_tickets.take();
        let keep_going = || self.album_scan_tickets.is_current(ticket);
        let (stats, album_ids) = if query.needs_play_stats() {
            tokio::try_join!(
                self.album_play_stats(&query.user_id, false, &keep_going),
                self.album_grid_scan(query, &keep_going),
            )?
        } else {
            (
                HashMap::new(),
                self.album_grid_scan(query, &keep_going).await?,
            )
        };
        let mut ids = select_grid_albums(album_ids, &stats, query.played, query.sort);
        if let Some(seed) = seed {
            // Sorted first, so the same albums always shuffle the same way,
            // whatever order the server listed albums of the same name in.
            ids.sort_unstable();
            shuffle_seeded(&mut ids, seed);
        }
        let ids = Arc::new(ids);
        cache_scan(
            &self.album_scan_cache,
            &self.album_scan_tickets,
            ticket,
            key,
            ids.clone(),
        );
        Ok(ids)
    }

    /// The seed of the shuffle the random grid `query` pages through. Its
    /// first page draws a new one — reloading the grid is a fresh shuffle —
    /// and later pages reuse it, so a rescan (the scan cache expired, or
    /// another grid took its place) lays the same albums out in the same order
    /// instead of reshuffling mid-list.
    fn random_grid_seed(&self, query: &AlbumGridQuery, first_page: bool) -> u64 {
        let mut shuffle = self.random_grid_shuffle.lock().unwrap();
        match shuffle.as_ref() {
            Some((key, seed)) if !first_page && key == query => *seed,
            _ => {
                let seed = uuid::Uuid::new_v4().as_u64_pair().0;
                *shuffle = Some((query.clone(), seed));
                seed
            }
        }
    }

    /// Ids of every album passing the grid's server-side filters, in the
    /// server order the grid builds on. A random grid is scanned in name order
    /// and shuffled afterwards: `sortBy=Random` reshuffles on every page
    /// request, so its pages would overlap and miss albums.
    async fn album_grid_scan(
        &self,
        query: &AlbumGridQuery,
        keep_going: &(dyn Fn() -> bool + Sync),
    ) -> AppResult<Vec<String>> {
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
        self.scan_items(
            &params,
            |items| {
                ids.extend(
                    items
                        .into_iter()
                        .map(|item| item.id)
                        .filter(|id| seen.insert(id.clone())),
                );
            },
            keep_going,
        )
        .await?;
        Ok(ids)
    }

    /// Per-album play stats of `user_id`, folded from a scan of every played
    /// track — the only items playback writes play data to. With
    /// `favorite_tracks`, of the played tracks the user favorited.
    async fn album_play_stats(
        &self,
        user_id: &str,
        favorite_tracks: bool,
        keep_going: &(dyn Fn() -> bool + Sync),
    ) -> AppResult<HashMap<String, AlbumPlayStats>> {
        let filters = if favorite_tracks {
            "IsFavorite,IsPlayed"
        } else {
            "IsPlayed"
        };
        let params = [
            ("userId", user_id.to_string()),
            ("includeItemTypes", "Audio".to_string()),
            ("recursive", "true".to_string()),
            ("filters", filters.to_string()),
            ("sortBy", "SortName".to_string()),
            ("sortOrder", "Ascending".to_string()),
            ("enableImages", "false".to_string()),
            ("enableUserData", "true".to_string()),
        ];
        let mut aggregate = AlbumPlayAggregate::default();
        self.scan_items(&params, |items| aggregate.add(items), keep_going)
            .await?;
        Ok(aggregate.albums)
    }

    /// Walk every page of an `/Items` query, [`ALBUM_SCAN_BATCH`] items at a
    /// time, handing each page to `take` so a caller keeps only what it needs.
    /// Stops with [`superseded`] before any page once `keep_going` says no.
    async fn scan_items(
        &self,
        query: &[(&'static str, String)],
        mut take: impl FnMut(Vec<BaseItem>),
        keep_going: &(dyn Fn() -> bool + Sync),
    ) -> AppResult<()> {
        let mut offset = 0u32;
        let mut scanned = 0usize;
        loop {
            if !keep_going() {
                return Err(superseded());
            }
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
            .ok_or_else(|| not_found("album", album_id))
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
        read_json(response).await
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
        let auth: AuthenticationResult = read_json(response).await?;
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
                    // The grid's favorite toggle reads IsFavorite from it.
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
                    ("enableUserData", "true".into()),
                ],
            )
            .await?;
        response
            .items
            .into_iter()
            .next()
            .map(ArtistDto::from_item)
            .ok_or_else(|| not_found("artist", artist_id))
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
    /// At most two channels (`maxAudioChannels`, which also sets the channel
    /// count of a transcode): a surround source comes as a stereo downmix.
    /// Our Opus decoder takes mono and stereo only, and played as it is, the
    /// device mix would keep the first two channels and drop the centre with
    /// the vocals in it.
    ///
    /// Without a `playSessionId`: that one belongs to an attempt at playing
    /// the track, not to the track, and is appended when a source is opened
    /// (`player/source.rs`). It is what the server correlates our playback
    /// reports with its transcode session by, so skipping a track kills its
    /// ffmpeg job.
    pub fn stream_url(&self, item_id: &str) -> AppResult<String> {
        let user_id = self.user_id()?;
        Ok(format!(
            "{}/Audio/{item_id}/universal?userId={user_id}&deviceId={}&container=opus,ogg%7Copus,flac,mp3,ogg%7Cvorbis,wav&transcodingContainer=opus&transcodingProtocol=http&audioCodec=opus&maxStreamingBitrate=320000000&maxAudioChannels=2&transcodingAudioChannels=2",
            self.base_url, self.device_id
        ))
    }

    /// Ceiling for one track fetched for the sonic analysis.
    pub(crate) const MAX_ANALYSIS_BYTES: u64 = 64 * 1024 * 1024;
    /// Longest track the sonic analysis fetches: its 96 kbit/s transcode is
    /// ~58 MB, under [`Self::MAX_ANALYSIS_BYTES`]. Longer is a mix, not a
    /// track to compare.
    pub const MAX_ANALYSIS_MS: u64 = 80 * 60 * 1000;

    /// The `maxStreamingBitrate` a track of `duration_ms` is fetched with
    /// for the analysis: a lossy original comes as it is only while even
    /// 320 kbit/s of it fits [`Self::MAX_ANALYSIS_BYTES`] (about 28 minutes);
    /// a longer track always comes as the 96 kbit/s transcode.
    pub(crate) fn analysis_bitrate(duration_ms: u64) -> u32 {
        if duration_ms.saturating_mul(320_000 / 8) / 1000 > Self::MAX_ANALYSIS_BYTES {
            96_000
        } else {
            320_000
        }
    }

    /// The whole track as a small file, for the library's sonic analysis
    /// (`sonic_scan.rs`): lossy originals come as they are while they are
    /// small enough ([`Self::analysis_bitrate`]), everything else is
    /// transcoded to 96 kbit/s Opus. The analysis works below 5 kHz, so
    /// nothing it looks at is lost, and a library of FLAC does not have to
    /// cross the network at full size. Stereo at most, as for playback
    /// ([`Self::stream_url`]). `duration_ms` is the server's length of the
    /// track (0: unknown).
    pub async fn analysis_audio(&self, item_id: &str, duration_ms: u64) -> AppResult<Vec<u8>> {
        let user_id = self.user_id()?;
        let max_bitrate = Self::analysis_bitrate(duration_ms);
        let url = format!(
            "{}/Audio/{item_id}/universal?userId={user_id}&deviceId={}&container=opus,ogg%7Copus,mp3,ogg%7Cvorbis&transcodingContainer=opus&transcodingProtocol=http&audioCodec=opus&audioBitRate=96000&maxStreamingBitrate={max_bitrate}&maxAudioChannels=2&transcodingAudioChannels=2",
            self.base_url, self.device_id
        );
        let response = self
            .http
            .get(url)
            .header("Authorization", self.auth_header())
            // A long transcode takes a small server a while to produce.
            .timeout(Duration::from_secs(10 * 60).max(Duration::from_millis(duration_ms / 4)))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                message: error_message(response).await,
            });
        }
        read_capped(response, Self::MAX_ANALYSIS_BYTES, "track for analysis").await
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
        let bytes = read_capped(response, MAX_IMAGE_BYTES, "cover image").await?;
        // A login page, a captive portal or a proxy's fallback page answers
        // 200 as well. The bytes end up in the cover cache, which serves its
        // files as immutable until the image tag changes.
        if !looks_like_image(&bytes) {
            return Err(AppError::Other(
                "the server answered with something other than an image".into(),
            ));
        }
        Ok(bytes)
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
        let wire: LyricsResponse = read_json(response).await?;
        Ok(LyricsDto::from_wire(wire))
    }

    // --- Discovery (genre filter, alphabet scrubber, similar artists) ---

    /// Music genres, for the album-grid filter chips, the genres page, the
    /// smart-view picker and the Discover row.
    ///
    /// `/Genres` answers with music genres only below a music library. Asked
    /// without one it lists `Genre` items, which Jellyfin creates only for
    /// genres something other than music carries too (a video, or a playlist,
    /// which takes on its tracks' genres): a library of nothing but music had
    /// none at all, and their ids are not the ones tracks name. So each music
    /// library is asked, and the answers merged.
    pub async fn genres(&self) -> AppResult<Vec<GenreDto>> {
        let user_id = self.user_id()?.to_string();
        let libraries = self.music_library_ids().await?;
        let mut seen = HashSet::new();
        let mut genres = Vec::new();
        for library_id in &libraries {
            let response: ItemsResponse = self
                .get_json(
                    "/Genres",
                    &[
                        ("userId", user_id.clone()),
                        ("parentId", library_id.clone()),
                        // The genres of albums: the album grid filters by them.
                        ("includeItemTypes", "MusicAlbum".into()),
                        ("sortBy", "SortName".into()),
                        ("sortOrder", "Ascending".into()),
                        ("enableImages", "false".into()),
                    ],
                )
                .await?;
            // A music genre has one id across libraries.
            genres.extend(
                response
                    .items
                    .into_iter()
                    .filter(|item| seen.insert(item.id.clone()))
                    .map(GenreDto::from_item),
            );
        }
        if libraries.len() > 1 {
            // Each library's answer is in name order; the merged list must be too.
            genres.sort_by_cached_key(|genre| genre.name.to_lowercase());
        }
        Ok(genres)
    }

    /// The music libraries the user can see, hidden ones included: hiding a
    /// library from the home screen is no reason for its genres to vanish.
    async fn music_library_ids(&self) -> AppResult<Vec<String>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                "/UserViews",
                &[
                    ("userId", user_id),
                    ("includeHidden", "true".into()),
                    ("includeExternalContent", "false".into()),
                ],
            )
            .await?;
        Ok(music_libraries(response.items))
    }

    /// The genres among `ids`, with their names. For ids the genre list does
    /// not have: smart views saved before it listed music genres hold the ids
    /// of Jellyfin's `Genre` items — the same genres under other ids — and the
    /// name leads back to the listed one. Ids that are no genre, or that the
    /// server no longer has, are left out.
    pub async fn genres_by_id(&self, ids: &[String]) -> AppResult<Vec<GenreDto>> {
        let user_id = self.user_id()?.to_string();
        // An id from storage that is no item id would get the whole request
        // refused; a comma would split it into two.
        let ids = ids
            .iter()
            .filter(|id| is_item_id(id))
            .cloned()
            .collect::<Vec<_>>();
        let mut genres = Vec::new();
        for chunk in unique_ids(&ids).chunks(MAX_IDS_PER_REQUEST) {
            // A lookup by id reaches the genre items, which no library holds.
            let response: ItemsResponse = self
                .get_json(
                    "/Items",
                    &[
                        ("userId", user_id.clone()),
                        ("ids", chunk.join(",")),
                        ("enableImages", "false".into()),
                        ("enableUserData", "false".into()),
                    ],
                )
                .await?;
            genres.extend(
                response
                    .items
                    .into_iter()
                    .filter(|item| matches!(item.item_type.as_str(), "Genre" | "MusicGenre"))
                    .map(GenreDto::from_item),
            );
        }
        Ok(genres)
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
    /// refreshed on every request; pagination is therefore a fresh sample (the
    /// album grid's random sort pages one shuffle instead).
    pub async fn discover_random_albums(
        &self,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<AlbumDto>> {
        let query = AlbumGridQuery {
            user_id: self.user_id()?.to_string(),
            genre_id: None,
            sort: AlbumSort::Random,
            favorites_only: false,
            played: PlayedFilter::All,
        };
        self.album_grid_page(&query, start_index, limit).await
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
    /// How many items match, without fetching any of them: `limit=0` and the
    /// count the server puts in every items response.
    async fn count_items(&self, path: &str, extra: &[(&str, String)]) -> AppResult<i64> {
        let mut params: Vec<(&str, String)> = vec![
            ("userId", self.user_id()?.to_string()),
            ("recursive", "true".into()),
            ("limit", "0".into()),
            ("enableImages", "false".into()),
            ("enableUserData", "false".into()),
        ];
        params.extend(extra.iter().cloned());
        let response: ItemsResponse = self.get_json(path, &params).await?;
        Ok(response.total_record_count.max(0))
    }

    /// Tracks in the library, and how many of them the server counts as
    /// played.
    pub async fn audio_counts(&self) -> AppResult<(i64, i64)> {
        let total = self
            .count_items("/Items", &[("includeItemTypes", "Audio".into())])
            .await?;
        let played = self
            .count_items(
                "/Items",
                &[
                    ("includeItemTypes", "Audio".into()),
                    ("filters", "IsPlayed".into()),
                ],
            )
            .await?;
        Ok((total, played))
    }

    pub async fn album_count(&self) -> AppResult<i64> {
        self.count_items("/Items", &[("includeItemTypes", "MusicAlbum".into())])
            .await
    }

    /// Album artists, the same set the artists page browses.
    pub async fn artist_count(&self) -> AppResult<i64> {
        self.count_items("/Artists/AlbumArtists", &[]).await
    }

    /// Albums released in one decade, counted without fetching them.
    pub async fn decade_album_count(&self, start_year: i32) -> AppResult<i64> {
        if !(1..=9990).contains(&start_year) || start_year % 10 != 0 {
            return Err(AppError::Other(
                "startYear must be a decade between 10 and 9990".into(),
            ));
        }
        let years = (start_year..=start_year + 9)
            .map(|year| year.to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.count_items(
            "/Items",
            &[("includeItemTypes", "MusicAlbum".into()), ("years", years)],
        )
        .await
    }

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

    /// Played albums, the one heard longest ago first — ranked by each album's
    /// latest track play (see [`least_recently_played`]). Ordering the played
    /// tracks by DatePlayed and keeping each album's first one ranked it by
    /// its least recently played track instead: one old interlude put an album
    /// heard yesterday at the top. Play history belongs to tracks in Jellyfin,
    /// so querying BoxSet/Album user-data here would silently produce the
    /// wrong result.
    ///
    /// The ranking needs every played track. A first page scans them — unless
    /// `reuse_recent`: the Discover row, loaded on every visit and every
    /// library change, takes a ranking from the last few minutes. Later pages
    /// page through the cached one. One scan at a time: a call that had to
    /// wait for a scan takes its ranking, as fresh as one of its own.
    pub async fn long_not_heard_albums(
        &self,
        start_index: u32,
        limit: u32,
        reuse_recent: bool,
    ) -> AppResult<Page<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let asked_at = Instant::now();
        let ids = {
            let mut cache = self.long_not_heard_cache.lock().await;
            let reusable = cache.as_ref().and_then(|last| {
                if start_index > 0 || reuse_recent {
                    last.ids_for(&user_id, Instant::now(), LONG_NOT_HEARD_CACHE_TTL)
                } else {
                    // Only a scan that ended while this call waited for it.
                    (last.key == user_id && last.scanned_at >= asked_at).then(|| last.ids.clone())
                }
            });
            match reusable {
                Some(ids) => ids,
                None => {
                    let stats = self.album_play_stats(&user_id, false, &|| true).await?;
                    let ids = Arc::new(least_recently_played(&stats));
                    *cache = Some(ScanCache {
                        key: user_id,
                        scanned_at: Instant::now(),
                        ids: ids.clone(),
                    });
                    ids
                }
            }
        };
        Ok(Page {
            items: self
                .albums_by_ids(&page_of_ids(&ids, start_index, limit))
                .await?,
            total: ids.len() as i64,
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
        self.smart_track_items_while(filter, &|| true).await
    }

    /// [`Self::smart_track_items`], stopping with [`superseded`] before any
    /// page once `keep_going` says no.
    async fn smart_track_items_while(
        &self,
        filter: &SmartFilter,
        keep_going: &(dyn Fn() -> bool + Sync),
    ) -> AppResult<Vec<BaseItem>> {
        let plan = smart_query_plan(filter)?;
        let mut offset = 0u32;
        let mut scanned = 0usize;
        let mut matches = SmartMatches::default();
        loop {
            if !keep_going() {
                return Err(superseded());
            }
            let query = self.smart_items_query(&plan, offset, SMART_SCAN_BATCH)?;
            let response: ItemsResponse = self.get_json("/Items", &query).await?;
            let received = response.items.len() as u32;
            let past_cutoff = filter
                .added_since
                .as_deref()
                .is_some_and(|since| reached_added_cutoff(&response.items, since));
            matches.add(response.items, filter);
            offset = offset.saturating_add(received);
            // Same bounds as `scan_items`: `total_record_count` is a
            // server-supplied number and must not be the only stop
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
        Ok(matches.items)
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

        let ticket = self.smart_scan_tickets.take();
        let items = self
            .smart_track_items_while(filter, &|| self.smart_scan_tickets.is_current(ticket))
            .await?;
        let ids = Arc::new(items.iter().map(|item| item.id.clone()).collect::<Vec<_>>());
        let total = ids.len() as i64;
        cache_scan(
            &self.smart_scan_cache,
            &self.smart_scan_tickets,
            ticket,
            filter.clone(),
            ids,
        );
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
    ///
    /// Several albums are in flight at once: one after the other, a large
    /// collection waited out a full round trip per album before anything
    /// played. [`ALBUM_TRACKS_IN_FLIGHT`] keeps a home server from being
    /// flooded, and `buffered` hands the results back in caller order.
    pub async fn tracks_for_albums(&self, album_ids: &[String]) -> AppResult<Vec<TrackDto>> {
        use futures_util::{StreamExt, TryStreamExt};

        if album_ids.iter().any(|album_id| album_id.trim().is_empty()) {
            return Err(AppError::Other("album id must not be empty".into()));
        }
        // The requests are made up front (an async fn's future starts nothing
        // until polled): a `StreamExt::map` closure over the borrowed ids is
        // not provably `Send` for every lifetime, which the commands need.
        let requests = album_ids
            .iter()
            .map(|album_id| self.album_tracks(album_id))
            .collect::<Vec<_>>();
        let per_album: Vec<Vec<TrackDto>> = futures_util::stream::iter(requests)
            .buffered(ALBUM_TRACKS_IN_FLIGHT)
            .try_collect()
            .await?;
        Ok(flatten_album_tracks(per_album))
    }

    // --- Read-only Jellyfin Collections / BoxSets ---

    /// One page of the box sets that hold music albums, each with its album
    /// count. Jellyfin has no filter for that, and a server with a movie
    /// library usually has plenty of movie collections, which were listed
    /// with their movie count shown as albums and failed to play. So every box
    /// set is listed, its music albums are counted, and the ones without are
    /// left out.
    pub async fn collections(
        self: &Arc<Self>,
        start_index: u32,
        limit: u32,
    ) -> AppResult<Page<CollectionDto>> {
        let user_id = self.user_id()?.to_string();
        let params = [
            ("userId", user_id),
            ("includeItemTypes", "BoxSet".to_string()),
            ("recursive", "true".to_string()),
            ("collapseBoxSetItems", "false".to_string()),
            ("sortBy", "SortName".to_string()),
            ("sortOrder", "Ascending".to_string()),
            ("fields", "ChildCount".to_string()),
            ("enableImages", "true".to_string()),
            ("imageTypeLimit", "1".to_string()),
        ];
        let mut sets = Vec::new();
        self.scan_items(&params, |items| sets.extend(items), &|| true)
            .await?;
        let (counts, failure) = self.collection_album_counts(&sets).await?;
        // A set whose count failed is left out this time; one timeout among a
        // mixed server's movie collections must not hide the music ones.
        let music = sets
            .into_iter()
            .zip(counts)
            .filter_map(|(set, albums)| {
                albums
                    .filter(|albums| *albums > 0)
                    .map(|albums| CollectionDto {
                        album_count: i32::try_from(albums).unwrap_or(i32::MAX),
                        ..CollectionDto::from_item(set)
                    })
            })
            .collect::<Vec<_>>();
        // With nothing to show, the failure is the answer, not "no collections".
        if let (true, Some(failure)) = (music.is_empty(), failure) {
            return Err(failure);
        }
        Ok(Page {
            total: music.len() as i64,
            items: music
                .into_iter()
                .skip(start_index as usize)
                .take(limit as usize)
                .collect(),
        })
    }

    /// The music-album count of each box set in `sets`, in order — `None`
    /// where counting failed, with the first such failure. A count is reused
    /// while the set's member count is the one it was taken at (and the count
    /// is recent), so the movie collections of a mixed server are asked about
    /// once rather than on every visit; the rest are counted a few at a time.
    /// Every count that succeeds is kept, one that failed is asked again next
    /// time. A 401 stops the counting and is returned instead: the session may
    /// have expired, and `with_retry` renews it only on an error.
    async fn collection_album_counts(
        self: &Arc<Self>,
        sets: &[BaseItem],
    ) -> AppResult<(Vec<Option<i64>>, Option<AppError>)> {
        let now = Instant::now();
        let mut counts = {
            let known = self.collection_counts.lock().unwrap();
            sets.iter()
                .map(|set| {
                    known.get(&set.id).copied().filter(|count| {
                        count.members == set.child_count.unwrap_or(0)
                            && now.saturating_duration_since(count.counted_at)
                                <= COLLECTION_COUNT_TTL
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut missing = counts
            .iter()
            .enumerate()
            .filter(|(_, count)| count.is_none())
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
            .into_iter();
        let mut jobs = tokio::task::JoinSet::new();
        let mut failure = None;
        let mut unauthorized = None;
        loop {
            while jobs.len() < COLLECTION_COUNT_CONCURRENCY {
                let Some(index) = missing.next() else { break };
                let client = Arc::clone(self);
                let set_id = sets[index].id.clone();
                jobs.spawn(async move {
                    // The same membership the collection page shows.
                    let albums = client
                        .count_items(
                            "/Items",
                            &[
                                ("parentId", set_id),
                                ("includeItemTypes", "MusicAlbum".into()),
                                ("collapseBoxSetItems", "false".into()),
                            ],
                        )
                        .await;
                    (index, albums)
                });
            }
            let Some(joined) = jobs.join_next().await else {
                break;
            };
            match joined {
                Ok((index, Ok(albums))) => {
                    counts[index] = Some(CollectionCount {
                        members: sets[index].child_count.unwrap_or(0),
                        albums,
                        counted_at: Instant::now(),
                    });
                }
                Ok((_, Err(error @ AppError::Server { status: 401, .. }))) => {
                    jobs.abort_all();
                    unauthorized = Some(error);
                    break;
                }
                Ok((_, Err(error))) => {
                    tracing::warn!("could not count the albums of a collection: {error}");
                    failure.get_or_insert(error);
                }
                Err(error) => {
                    tracing::warn!("counting the albums of a collection failed: {error}");
                    failure.get_or_insert(AppError::Other(format!(
                        "counting a collection failed: {error}"
                    )));
                }
            }
        }
        // Box sets that are gone are forgotten, uncounted ones asked again.
        *self.collection_counts.lock().unwrap() = sets
            .iter()
            .zip(&counts)
            .filter_map(|(set, count)| count.map(|count| (set.id.clone(), count)))
            .collect();
        if let Some(error) = unauthorized {
            return Err(error);
        }
        Ok((
            counts
                .into_iter()
                .map(|count| count.map(|count| count.albums))
                .collect(),
            failure,
        ))
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
            .ok_or_else(|| not_found("collection", collection_id))
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
    ///
    /// Bounded by [`LOGOUT_TIMEOUT`], not the client's 30 s: the sign-out
    /// waits for this (holding the re-login gate), and a stalled server must
    /// not keep the user looking at a button that seems to do nothing.
    pub async fn logout(&self) -> AppResult<()> {
        let url = format!("{}/Sessions/Logout", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .timeout(LOGOUT_TIMEOUT)
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
            .ok_or_else(|| not_found("playlist", playlist_id))
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

    /// Duplicate a playlist from a fresh source snapshot, its tracks in source
    /// order. A track the source holds twice is in the copy once: Jellyfin
    /// 10.11 creates a playlist from distinct ids.
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

    /// One request, so the playlist is created whole or not at all — which
    /// also keeps a re-login retry from making a second one. Its timeout grows
    /// with the list (see [`create_playlist_timeout`]).
    pub async fn create_playlist(&self, name: &str, track_ids: &[String]) -> AppResult<String> {
        let user_id = self.user_id()?.to_string();
        let url = format!("{}/Playlists", self.base_url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .timeout(create_playlist_timeout(track_ids.len()))
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
        let created: CreatePlaylistResult = read_json(response).await?;
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

    /// The item id of every playlist entry, in playlist order and whatever its
    /// media type — for rewriting the playlist, where an entry left out is an
    /// entry lost.
    async fn playlist_entry_item_ids(&self, playlist_id: &str) -> AppResult<Vec<String>> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                &format!("/Playlists/{playlist_id}/Items"),
                &[
                    ("userId", user_id),
                    ("enableImages", "false".into()),
                    ("enableUserData", "false".into()),
                ],
            )
            .await?;
        Ok(response.items.into_iter().map(|item| item.id).collect())
    }

    /// The item id of every entry the playlist holds, in order — with the ones
    /// [`Self::playlist_entry_item_ids`] leaves out because the user may not
    /// see them (a library they cannot open, a rating above their limit).
    async fn playlist_stored_item_ids(&self, playlist_id: &str) -> AppResult<Vec<String>> {
        let info: PlaylistInfoWire = self
            .get_json(&format!("/Playlists/{playlist_id}"), &[])
            .await?;
        Ok(info.item_ids)
    }

    /// Drop the later entries of every item the playlist holds more than once,
    /// keeping the first.
    ///
    /// Jellyfin 10.11 cannot remove one entry of a repeated track: the entry
    /// id is the track's id, and removing by it takes every copy, the one to
    /// keep included. So the playlist is rewritten with each item once, in
    /// order — `Ids` on `POST /Playlists/{id}` replaces the entries. Built from
    /// a fresh listing, so repeating it is harmless.
    ///
    /// The rewrite keeps only what the user may see: the server re-adds the
    /// ids through the same visibility check that keeps hidden entries out of
    /// the listing. A shared playlist can hold tracks a collaborator added from
    /// a library this user cannot open; rewriting would delete them. So when
    /// the playlist holds entries the listing does not, nothing is changed and
    /// their number is reported. (Links to items that are gone are in neither
    /// list; any edit of the playlist drops them, Jellyfin's removal too.)
    pub async fn remove_playlist_duplicates(
        &self,
        playlist_id: &str,
    ) -> AppResult<PlaylistCleanup> {
        if playlist_id.trim().is_empty() {
            return Err(AppError::Other("playlist id must not be empty".into()));
        }
        // Everything first, then what the user sees: an entry removed in
        // between then counts as hidden instead of coming back with the
        // rewrite.
        let stored = self.playlist_stored_item_ids(playlist_id).await?;
        let entries = self.playlist_entry_item_ids(playlist_id).await?;
        let kept = unique_ids(&entries);
        let removed = entries.len() - kept.len();
        if removed == 0 {
            return Ok(PlaylistCleanup::default());
        }
        let hidden = hidden_entries(&stored, &entries);
        if hidden > 0 {
            return Ok(PlaylistCleanup { removed: 0, hidden });
        }
        self.post_no_content(
            &format!("/Playlists/{playlist_id}"),
            serde_json::json!({ "Ids": kept }),
        )
        .await?;
        Ok(PlaylistCleanup { removed, hidden: 0 })
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

    /// Remove playlist entries. `entry_ids` are PlaylistItemIds, NOT item ids —
    /// though Jellyfin 10.11 makes them the same, and removes every entry
    /// whose id matches: all copies of a track that is in the playlist twice.
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
    /// (remove-then-insert semantics, like the queue). The index counts every
    /// entry the user can see, non-audio ones included — an index into
    /// `playlist_tracks` goes through `playlist_move_audio`. Jellyfin 10.11
    /// moves the first entry with that id, and places it after the first entry
    /// of the track before `new_index` — wrong for a repeated track.
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

    /// Move a playlist entry to `audio_index` among the playlist's audio
    /// entries, the list `playlist_tracks` returns. That list hides the
    /// non-audio entries Jellyfin's own index counts, so the index is worked
    /// out against a fresh listing of the whole playlist first.
    pub async fn playlist_move_audio(
        &self,
        playlist_id: &str,
        entry_id: &str,
        audio_index: usize,
    ) -> AppResult<()> {
        let user_id = self.user_id()?.to_string();
        let response: ItemsResponse = self
            .get_json(
                &format!("/Playlists/{playlist_id}/Items"),
                &[
                    ("userId", user_id),
                    ("enableImages", "false".into()),
                    ("enableUserData", "false".into()),
                ],
            )
            .await?;
        // The same test `playlist_tracks` filters with.
        let entries: Vec<(&str, bool)> = response
            .items
            .iter()
            .map(|item| {
                (
                    item.playlist_item_id.as_deref().unwrap_or_default(),
                    item.item_type == "Audio",
                )
            })
            .collect();
        let index = playlist_move_index(&entries, entry_id, audio_index)
            .ok_or_else(|| AppError::Other(format!("playlist entry not found: {entry_id}")))?;
        self.playlist_move(playlist_id, entry_id, index).await
    }

    /// Album row for the home page: newest/most/last played depending on
    /// `sort_by` ("DateCreated" / "PlayCount" / "DatePlayed").
    ///
    /// Playback only updates Played/PlayCount/DatePlayed on the *track*
    /// user-data, never on the album — so the played-based rows query tracks
    /// and map them to their albums (jellyfin-web does the same). An album
    /// comes where its first track does: at its latest play, or at its most
    /// played track.
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

        let query = [
            ("userId", user_id),
            ("includeItemTypes", "Audio".to_string()),
            ("recursive", "true".to_string()),
            // The server sorts never-played tracks (no DatePlayed, PlayCount
            // 0) after the played ones, but it still returns them: without
            // the filter, an account with a few plays had its "most played"
            // padded with albums nobody played.
            ("filters", "IsPlayed".to_string()),
            // A tie-break of its own keeps the pages of the walk in one order.
            ("sortBy", format!("{sort_by},SortName")),
            ("sortOrder", "Descending,Ascending".to_string()),
            ("enableImages", "false".to_string()),
            ("enableUserData", "false".to_string()),
        ];
        let album_ids = self.leading_album_ids(&query, limit).await?;
        self.albums_by_ids(&album_ids).await
    }

    /// The first `limit` distinct albums of the tracks `query` lists, in the
    /// order their first track comes. Walks the tracks page by page until it
    /// has them: someone who plays whole albums has a dozen tracks of each at
    /// the top, and the one fixed sample this used to take left the row short.
    async fn leading_album_ids(
        &self,
        query: &[(&'static str, String)],
        limit: u32,
    ) -> AppResult<Vec<String>> {
        let mut albums = Vec::new();
        let mut offset = 0u32;
        while offset < HOME_TRACK_MAX {
            let mut page = query.to_vec();
            page.push(("startIndex", offset.to_string()));
            page.push(("limit", HOME_TRACK_BATCH.to_string()));
            let response: ItemsResponse = self.get_json("/Items", &page).await?;
            let received = response.items.len() as u32;
            let total = response.total_record_count;
            if take_new_albums(&mut albums, response.items, limit as usize) {
                break;
            }
            offset = offset.saturating_add(received);
            if received < HOME_TRACK_BATCH || i64::from(offset) >= total {
                break;
            }
        }
        Ok(albums)
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

    /// "Forgotten favorites": the albums of favorited tracks you have played,
    /// the one whose favorites you heard longest ago first. Like "long not
    /// heard", an album ranks by its latest favorite play — a favorite played
    /// yesterday means its album's favorites are not forgotten. That takes
    /// every played favorite, but favorites are few.
    pub async fn forgotten_favorite_albums(&self, limit: u32) -> AppResult<Vec<AlbumDto>> {
        let user_id = self.user_id()?.to_string();
        let stats = self.album_play_stats(&user_id, true, &|| true).await?;
        let album_ids = page_of_ids(&least_recently_played(&stats), 0, limit);
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
    fn a_repeated_track_on_jellyfin_10_11_resolves_once() {
        // 10.11 makes every entry id the track's own id, so both entries of a
        // track that is in the playlist twice carry the same one.
        let source = [
            ("track-a", "track-a"),
            ("track-b", "track-b"),
            ("track-a", "track-a"),
        ];
        let resolved = resolve_playlist_item_ids(source, &ids(&["track-a"])).expect("selection");
        assert_eq!(resolved, ids(&["track-a"]));
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
mod playlist_move_tests {
    use super::*;

    /// Jellyfin's move: take the entry out, put it back at `index` (the end
    /// when past it).
    fn server_move(entries: &mut Vec<(&str, bool)>, entry_id: &str, index: usize) {
        let from = entries.iter().position(|(id, _)| *id == entry_id).unwrap();
        let entry = entries.remove(from);
        entries.insert(index.min(entries.len()), entry);
    }

    fn audio_order<'a>(entries: &[(&'a str, bool)]) -> Vec<&'a str> {
        entries
            .iter()
            .filter(|(_, audio)| *audio)
            .map(|(id, _)| *id)
            .collect()
    }

    #[test]
    fn a_hidden_entry_before_the_target_shifts_the_server_index() {
        // V is a video the playlist page never shows. Dragging A below C is
        // audio index 2; sent as is, Jellyfin would put A between B and C.
        let entries = [("V", false), ("A", true), ("B", true), ("C", true)];
        let index = playlist_move_index(&entries, "A", 2).unwrap();
        assert_eq!(index, 3);

        let mut moved = entries.to_vec();
        server_move(&mut moved, "A", index);
        assert_eq!(moved, [("V", false), ("B", true), ("C", true), ("A", true)]);
    }

    #[test]
    fn every_move_lands_where_the_page_put_it() {
        // Every layout of six entries with at least two audio ones, every
        // drag the page can make: the server's audio order must end up as the
        // page's optimistic one, and the hidden entries keep their order.
        let names = ["e0", "e1", "e2", "e3", "e4", "e5"];
        for layout in 0u32..(1 << names.len()) {
            let entries: Vec<(&str, bool)> = names
                .iter()
                .enumerate()
                .map(|(bit, id)| (*id, layout & (1 << bit) != 0))
                .collect();
            let audio = audio_order(&entries);
            if audio.len() < 2 {
                continue;
            }
            let hidden: Vec<&str> = entries
                .iter()
                .filter(|(_, audio)| !*audio)
                .map(|(id, _)| *id)
                .collect();
            for from in 0..audio.len() {
                for to in 0..audio.len() {
                    let mut expected = audio.clone();
                    let dragged = expected.remove(from);
                    expected.insert(to, dragged);

                    let index = playlist_move_index(&entries, dragged, to).unwrap();
                    let mut moved = entries.clone();
                    server_move(&mut moved, dragged, index);

                    let case = format!("layout {layout:06b}, {dragged} to {to}");
                    assert_eq!(audio_order(&moved), expected, "{case}");
                    let hidden_after: Vec<&str> = moved
                        .iter()
                        .filter(|(_, audio)| !*audio)
                        .map(|(id, _)| *id)
                        .collect();
                    assert_eq!(hidden_after, hidden, "{case}");
                }
            }
        }
    }

    #[test]
    fn an_all_audio_playlist_keeps_the_index() {
        let entries = [("A", true), ("B", true), ("C", true), ("D", true)];
        for (id, from) in [("A", 0), ("B", 1), ("C", 2), ("D", 3)] {
            for to in 0..entries.len() {
                assert_eq!(
                    playlist_move_index(&entries, id, to),
                    Some(to),
                    "{id} from {from}"
                );
            }
        }
    }

    #[test]
    fn a_target_past_the_end_lands_after_the_last_audio_entry() {
        let entries = [("A", true), ("B", true), ("V", false)];
        assert_eq!(playlist_move_index(&entries, "A", 9), Some(1));
    }

    #[test]
    fn an_unknown_entry_has_no_index() {
        let entries = [("A", true), ("B", true)];
        assert_eq!(playlist_move_index(&entries, "gone", 0), None);
        assert_eq!(playlist_move_index(&[], "gone", 0), None);
    }

    #[test]
    fn a_repeated_entry_id_moves_its_first_copy_like_the_server() {
        // Jellyfin 10.11 hands out the track id as the entry id, so a track
        // that is in a playlist twice has one id twice; the server moves the
        // first match.
        let entries = [("T", true), ("V", false), ("B", true), ("T", true)];
        let index = playlist_move_index(&entries, "T", 1).unwrap();
        let mut moved = entries.to_vec();
        server_move(&mut moved, "T", index);
        assert_eq!(audio_order(&moved), ["B", "T", "T"]);
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
            // The track names its music genre, whose id is not the one the
            // filter carries for the same name; the server matched the genre.
            genre_items: vec![NameGuidPair {
                id: "music-genre-rock".into(),
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
    fn only_grids_without_play_data_or_random_order_page_on_the_server() {
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
            // The server would reshuffle every page.
            query(AlbumSort::Random, PlayedFilter::All),
        ] {
            assert!(!local.is_server_side(), "{local:?}");
        }
    }

    #[test]
    fn only_play_sorts_and_the_played_filter_scan_the_play_history() {
        assert!(!query(AlbumSort::Random, PlayedFilter::All).needs_play_stats());
        assert!(!query(AlbumSort::Name, PlayedFilter::All).needs_play_stats());
        assert!(query(AlbumSort::Random, PlayedFilter::Unplayed).needs_play_stats());
        assert!(query(AlbumSort::DatePlayed, PlayedFilter::All).needs_play_stats());
    }

    #[test]
    fn a_seeded_shuffle_repeats_itself_and_keeps_every_album() {
        let albums = (0..40).map(|n| format!("album-{n:02}")).collect::<Vec<_>>();
        let mut first = albums.clone();
        shuffle_seeded(&mut first, 7);
        let mut again = albums.clone();
        shuffle_seeded(&mut again, 7);
        assert_eq!(first, again, "the same seed, the same order");
        assert_ne!(first, albums, "it does shuffle");

        let mut other = albums.clone();
        shuffle_seeded(&mut other, 8);
        assert_ne!(first, other, "another seed, another order");

        first.sort();
        assert_eq!(
            first, albums,
            "a permutation: nothing lost, nothing doubled"
        );

        let mut empty: Vec<String> = Vec::new();
        shuffle_seeded(&mut empty, 7);
        let mut one = ids(&["a"]);
        shuffle_seeded(&mut one, 7);
        assert_eq!(one, ids(&["a"]));
    }

    #[test]
    fn long_not_heard_ranks_an_album_by_its_latest_play() {
        let mut aggregate = AlbumPlayAggregate::default();
        aggregate.add([
            // Album "yesterday": mostly played yesterday, one interlude in 2022.
            played_track(
                "y1",
                Some("yesterday"),
                4,
                Some("2026-10-19T20:00:00.0000000Z"),
            ),
            played_track(
                "y2",
                Some("yesterday"),
                1,
                Some("2022-01-01T10:00:00.0000000Z"),
            ),
            // Album "spring": every track last played in March 2024.
            played_track(
                "s1",
                Some("spring"),
                2,
                Some("2024-03-01T08:00:00.0000000Z"),
            ),
            played_track(
                "s2",
                Some("spring"),
                2,
                Some("2024-03-02T08:00:00.0000000Z"),
            ),
            // Marked played without a date: nowhere to place it but last.
            played_track("u1", Some("undated"), 0, None),
            played_track("o1", Some("old"), 1, Some("2019-06-01T08:00:00.0000000Z")),
        ]);
        assert_eq!(
            least_recently_played(&aggregate.albums),
            ids(&["old", "spring", "yesterday", "undated"])
        );
        assert!(least_recently_played(&HashMap::new()).is_empty());
    }

    #[test]
    fn a_home_row_takes_new_albums_until_it_is_full() {
        let tracks = |albums: &[Option<&str>]| {
            albums
                .iter()
                .map(|album| BaseItem {
                    album_id: album.map(str::to_string),
                    ..BaseItem::default()
                })
                .collect::<Vec<_>>()
        };
        let mut row = Vec::new();
        // A whole album played through: one album, however many tracks.
        assert!(!take_new_albums(
            &mut row,
            tracks(&[Some("a"), Some("a"), None, Some(""), Some("a")]),
            3
        ));
        assert_eq!(row, ids(&["a"]));
        // The next page continues the same row.
        assert!(take_new_albums(
            &mut row,
            tracks(&[Some("b"), Some("a"), Some("c"), Some("d")]),
            3
        ));
        assert_eq!(row, ids(&["a", "b", "c"]));
    }

    #[test]
    fn genres_come_from_the_music_libraries() {
        let view = |id: &str, kind: Option<&str>| BaseItem {
            id: id.into(),
            collection_type: kind.map(str::to_string),
            ..BaseItem::default()
        };
        assert_eq!(
            music_libraries(vec![
                view("movies", Some("movies")),
                view("music", Some("music")),
                view("playlists", Some("playlists")),
                view("folder", None),
                view("music-2", Some("Music")),
                view("", Some("music")),
            ]),
            ids(&["music", "music-2"])
        );
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

/// The library calls against a local stand-in that answers like Jellyfin 10.11
/// and records every request line.
#[cfg(test)]
mod library_flow_tests {
    use super::*;
    use std::io::{Read, Write};

    type Requests = Arc<Mutex<Vec<String>>>;

    /// A local HTTP server answering each request with `route(request line)`
    /// as JSON, one connection at a time. Records the request line, followed
    /// by the body when there is one.
    fn server(route: impl Fn(&str) -> String + Send + 'static) -> (String, Requests) {
        server_with_status(move |line| (200, route(line)))
    }

    /// [`server`], with the status of every answer picked by `route` too.
    fn server_with_status(
        route: impl Fn(&str) -> (u16, String) + Send + 'static,
    ) -> (String, Requests) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let url = format!("http://{}", listener.local_addr().expect("server address"));
        let requests = Requests::default();
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut data = Vec::new();
                let mut buf = [0u8; 1024];
                let head_end = loop {
                    if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break data.len(),
                        Ok(n) => data.extend_from_slice(&buf[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&data[..head_end]).to_string();
                let length = head
                    .lines()
                    .filter_map(|header| header.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while data.len() < head_end + length {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => data.extend_from_slice(&buf[..n]),
                    }
                }
                let line = head.lines().next().unwrap_or_default().to_string();
                let (status, body) = route(&line);
                let sent = String::from_utf8_lossy(&data[head_end..]);
                seen.lock()
                    .unwrap()
                    .push(format!("{line} {sent}").trim_end().to_string());
                let response = format!(
                    "HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (url, requests)
    }

    fn client(url: &str) -> Arc<JellyfinClient> {
        Arc::new(
            JellyfinClient::new(url, "test-device", Vec::new())
                .expect("client")
                .with_session("session".into(), "user".into()),
        )
    }

    fn hits(requests: &Requests, needle: &str) -> usize {
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    #[tokio::test]
    async fn genres_are_the_music_genres_of_every_music_library() {
        let (url, requests) = server(|line| {
            if line.contains("/UserViews") {
                r#"{"Items":[
                    {"Id":"lib-a","Name":"Music","CollectionType":"music"},
                    {"Id":"movies","Name":"Movies","CollectionType":"movies"},
                    {"Id":"lib-b","Name":"Vinyl rips","CollectionType":"music"}]}"#
            } else if line.contains("parentId=lib-a") {
                r#"{"Items":[{"Id":"mg-rock","Name":"Rock"},{"Id":"mg-jazz","Name":"Jazz"}]}"#
            } else if line.contains("parentId=lib-b") {
                r#"{"Items":[{"Id":"mg-blues","Name":"Blues"},{"Id":"mg-rock","Name":"Rock"}]}"#
            } else {
                // `/Genres` without a music parent: never asked any more.
                r#"{"Items":[{"Id":"genre-rock","Name":"Rock"}]}"#
            }
            .to_string()
        });
        let genres = client(&url).genres().await.expect("genres");
        assert_eq!(
            genres.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(),
            ["mg-blues", "mg-jazz", "mg-rock"]
        );
        assert_eq!(hits(&requests, "/Genres"), 2);
        assert_eq!(hits(&requests, "parentId=movies"), 0);
        assert_eq!(hits(&requests, "includeHidden=true"), 1);
    }

    #[tokio::test]
    async fn genre_ids_resolve_to_their_genres_and_nothing_else() {
        // The `Genre` item an older smart view picked, and an id that names an
        // album instead.
        const ROCK: &str = "0123456789abcdef0123456789abcdef";
        const ALBUM: &str = "fedcba98-7654-3210-fedc-ba9876543210";
        let (url, requests) = server(|_| {
            r#"{"Items":[
                {"Id":"0123456789abcdef0123456789abcdef","Name":"Rock","Type":"Genre"},
                {"Id":"fedcba9876543210fedcba9876543210","Name":"An album","Type":"MusicAlbum"}]}"#
                .to_string()
        });
        let client = client(&url);
        let ids = [ROCK, ALBUM, "not-an-id", "a,b", "", ROCK].map(String::from);
        let genres = client.genres_by_id(&ids).await.expect("genres_by_id");
        assert_eq!(
            genres
                .iter()
                .map(|genre| (genre.id.as_str(), genre.name.as_str()))
                .collect::<Vec<_>>(),
            [(ROCK, "Rock")]
        );
        // One lookup by id, of the well-formed ids, each once.
        let lines = requests.lock().unwrap().clone();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains(&format!("ids={ROCK}%2C{ALBUM}&")),
            "{}",
            lines[0]
        );

        // Nothing well-formed: nothing to ask.
        let none = client
            .genres_by_id(&["junk".to_string()])
            .await
            .expect("genres_by_id");
        assert!(none.is_empty());
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn only_box_sets_with_music_albums_are_listed_and_counted_once() {
        let (url, requests) = server(|line| {
            if line.contains("includeItemTypes=BoxSet") {
                r#"{"Items":[
                    {"Id":"music-set","Name":"Quiet Hours","Type":"BoxSet","ChildCount":2},
                    {"Id":"movie-set","Name":"Film Night","Type":"BoxSet","ChildCount":4}],
                    "TotalRecordCount":2}"#
            } else if line.contains("parentId=music-set") {
                r#"{"Items":[],"TotalRecordCount":2}"#
            } else {
                r#"{"Items":[],"TotalRecordCount":0}"#
            }
            .to_string()
        });
        let client = client(&url);
        let page = client.collections(0, 10).await.expect("collections");
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].name, "Quiet Hours");
        // The music albums, not the members.
        assert_eq!(page.items[0].album_count, 2);
        assert_eq!(hits(&requests, "includeItemTypes=MusicAlbum"), 2);

        // Member counts unchanged: the next visit counts nothing again.
        let again = client.collections(0, 10).await.expect("collections");
        assert_eq!(again.items[0].album_count, 2);
        assert_eq!(hits(&requests, "includeItemTypes=MusicAlbum"), 2);
        assert!(client
            .collections(1, 10)
            .await
            .expect("page 2")
            .items
            .is_empty());
    }

    /// Two box sets, one with music, and the answer to counting the other's
    /// albums.
    fn box_sets_with_movie_count(movie_count: (u16, &'static str)) -> (String, Requests) {
        server_with_status(move |line| {
            if line.contains("includeItemTypes=BoxSet") {
                (
                    200,
                    r#"{"Items":[
                        {"Id":"music-set","Name":"Quiet Hours","Type":"BoxSet","ChildCount":2},
                        {"Id":"movie-set","Name":"Film Night","Type":"BoxSet","ChildCount":4}],
                        "TotalRecordCount":2}"#
                        .to_string(),
                )
            } else if line.contains("parentId=music-set") {
                (200, r#"{"Items":[],"TotalRecordCount":2}"#.to_string())
            } else {
                (movie_count.0, movie_count.1.to_string())
            }
        })
    }

    #[tokio::test]
    async fn a_box_set_that_cannot_be_counted_is_left_out_and_asked_again() {
        let (url, requests) = box_sets_with_movie_count((500, r#"{"error":"timeout"}"#));
        let client = client(&url);
        let page = client.collections(0, 10).await.expect("collections");
        // The music collection is there although the other count failed.
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].name, "Quiet Hours");
        assert_eq!(page.items[0].album_count, 2);

        // Its count was kept; the failed one is asked again.
        client.collections(0, 10).await.expect("collections");
        assert_eq!(hits(&requests, "parentId=music-set"), 1);
        assert_eq!(hits(&requests, "parentId=movie-set"), 2);
    }

    #[tokio::test]
    async fn collections_fail_only_when_nothing_could_be_shown() {
        // The one music collection fails to count: an empty page would claim
        // there are no collections.
        let (url, _) = server_with_status(|line| {
            if line.contains("includeItemTypes=BoxSet") {
                (
                    200,
                    r#"{"Items":[{"Id":"music-set","Name":"Quiet Hours","Type":"BoxSet","ChildCount":2}],
                        "TotalRecordCount":1}"#
                        .to_string(),
                )
            } else {
                (503, r#"{"error":"busy"}"#.to_string())
            }
        });
        let error = client(&url)
            .collections(0, 10)
            .await
            .expect_err("nothing could be counted");
        assert!(
            matches!(error, AppError::Server { status: 503, .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_401_while_counting_fails_the_page_for_a_renewed_session() {
        // `with_retry` renews an expired session only on an error.
        let (url, _) = box_sets_with_movie_count((401, ""));
        let error = client(&url)
            .collections(0, 10)
            .await
            .expect_err("the 401 comes through");
        assert!(
            matches!(error, AppError::Server { status: 401, .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn long_not_heard_scans_once_and_pages_through_the_ranking() {
        let (url, requests) = server(|line| {
            if line.contains("filters=IsPlayed") {
                // Album "fresh" was heard yesterday, apart from one old track.
                r#"{"Items":[
                    {"Id":"t1","AlbumId":"fresh","UserData":{"PlayCount":3,"LastPlayedDate":"2026-10-19T20:00:00.0000000Z"}},
                    {"Id":"t2","AlbumId":"fresh","UserData":{"PlayCount":1,"LastPlayedDate":"2021-01-01T08:00:00.0000000Z"}},
                    {"Id":"t3","AlbumId":"stale","UserData":{"PlayCount":1,"LastPlayedDate":"2024-03-01T08:00:00.0000000Z"}}],
                    "TotalRecordCount":3}"#
            } else {
                // The albums by id, in no particular order.
                r#"{"Items":[{"Id":"stale","Name":"Stale"},{"Id":"fresh","Name":"Fresh"}]}"#
            }
            .to_string()
        });
        let client = client(&url);
        let first = client
            .long_not_heard_albums(0, 1, false)
            .await
            .expect("page 1");
        assert_eq!(first.total, 2);
        assert_eq!(first.items[0].id, "stale");
        let second = client
            .long_not_heard_albums(1, 1, false)
            .await
            .expect("page 2");
        assert_eq!(second.items[0].id, "fresh");
        // The second page came from the first page's ranking.
        assert_eq!(hits(&requests, "filters=IsPlayed"), 1);
    }

    #[tokio::test]
    async fn the_discover_row_reuses_a_recent_ranking_and_scans_run_one_at_a_time() {
        let (url, requests) = server(|line| {
            if line.contains("filters=IsPlayed") {
                r#"{"Items":[{"Id":"t1","AlbumId":"a","UserData":{"PlayCount":1,"LastPlayedDate":"2024-03-01T08:00:00.0000000Z"}}],
                    "TotalRecordCount":1}"#
            } else {
                r#"{"Items":[{"Id":"a","Name":"A"}]}"#
            }
            .to_string()
        });
        let client = client(&url);
        let scans = || hits(&requests, "filters=IsPlayed");

        // The hub loads twice at once (mount and library change): one scan.
        let (one, two) = tokio::join!(
            client.long_not_heard_albums(0, 12, true),
            client.long_not_heard_albums(0, 12, true),
        );
        assert_eq!(one.expect("row").items[0].id, "a");
        assert_eq!(two.expect("row").items[0].id, "a");
        assert_eq!(scans(), 1);

        // The next visit takes that ranking.
        client
            .long_not_heard_albums(0, 12, true)
            .await
            .expect("row");
        assert_eq!(scans(), 1);

        // "See all" ranks afresh...
        client
            .long_not_heard_albums(0, 60, false)
            .await
            .expect("first page");
        assert_eq!(scans(), 2);

        // ...but a call that had to wait for a scan takes that one.
        let (one, two) = tokio::join!(
            client.long_not_heard_albums(0, 60, false),
            client.long_not_heard_albums(0, 60, false),
        );
        one.expect("first page");
        two.expect("first page");
        assert_eq!(scans(), 3);
    }

    #[tokio::test]
    async fn most_played_albums_come_from_played_tracks_only() {
        let (url, requests) = server(|line| {
            if line.contains("includeItemTypes=Audio") {
                // One played album, its tracks across the first page.
                r#"{"Items":[{"Id":"t1","AlbumId":"a"},{"Id":"t2","AlbumId":"a"}],"TotalRecordCount":2}"#
            } else {
                r#"{"Items":[{"Id":"a","Name":"A"}]}"#
            }
            .to_string()
        });
        let albums = client(&url)
            .home_albums("PlayCount", 12)
            .await
            .expect("most played");
        assert_eq!(albums.len(), 1);
        let walk = requests
            .lock()
            .unwrap()
            .iter()
            .find(|line| line.contains("includeItemTypes=Audio"))
            .cloned()
            .expect("the track walk");
        assert!(walk.contains("filters=IsPlayed"), "{walk}");
        assert!(walk.contains("sortBy=PlayCount%2CSortName"), "{walk}");
    }

    /// A playlist as 10.11 answers for it: `stored` from `GET /Playlists/pl`
    /// (every entry), `listed` from its `/Items` (what the user may see).
    fn playlist_server(stored: &'static str, listed: &'static str) -> (String, Requests) {
        server(move |line| {
            if line.starts_with("GET /Playlists/pl/Items") {
                listed.to_string()
            } else if line.starts_with("GET /Playlists/pl") {
                stored.to_string()
            } else {
                String::new()
            }
        })
    }

    #[tokio::test]
    async fn duplicate_cleanup_rewrites_the_playlist_with_every_track_once() {
        let (url, requests) = playlist_server(
            r#"{"OpenAccess":false,"Shares":[],"ItemIds":["a","v","a","b"]}"#,
            // As 10.11 lists a playlist with track "a" twice: one entry id.
            r#"{"Items":[
                {"Id":"a","Type":"Audio","PlaylistItemId":"a"},
                {"Id":"v","Type":"Video","PlaylistItemId":"v"},
                {"Id":"a","Type":"Audio","PlaylistItemId":"a"},
                {"Id":"b","Type":"Audio","PlaylistItemId":"b"}]}"#,
        );
        let client = client(&url);
        assert_eq!(
            client
                .remove_playlist_duplicates("pl")
                .await
                .expect("cleanup"),
            PlaylistCleanup {
                removed: 1,
                hidden: 0
            }
        );
        let rewrite = requests
            .lock()
            .unwrap()
            .iter()
            .find(|line| line.starts_with("POST /Playlists/pl "))
            .cloned()
            .expect("the rewrite");
        // Every entry once, in order — the video too: an entry left out is lost.
        assert!(rewrite.ends_with(r#"{"Ids":["a","v","b"]}"#), "{rewrite}");
        // Nothing removed by entry id, which would have taken both copies.
        assert_eq!(hits(&requests, "DELETE"), 0);
    }

    #[tokio::test]
    async fn a_playlist_without_duplicates_is_left_alone() {
        let (url, requests) = playlist_server(
            r#"{"ItemIds":["a","b"]}"#,
            r#"{"Items":[{"Id":"a","PlaylistItemId":"a"},{"Id":"b","PlaylistItemId":"b"}]}"#,
        );
        let cleanup = client(&url)
            .remove_playlist_duplicates("pl")
            .await
            .expect("cleanup");
        assert_eq!(cleanup, PlaylistCleanup::default());
        assert_eq!(hits(&requests, "POST"), 0);
    }

    #[tokio::test]
    async fn duplicates_stay_while_the_playlist_holds_entries_the_user_cannot_see() {
        // "h" came from a library this user cannot open: the listing leaves it
        // out, and a rewrite from the listing would delete it.
        let (url, requests) = playlist_server(
            r#"{"ItemIds":["a","h","a","b"]}"#,
            r#"{"Items":[
                {"Id":"a","PlaylistItemId":"a"},
                {"Id":"a","PlaylistItemId":"a"},
                {"Id":"b","PlaylistItemId":"b"}]}"#,
        );
        let cleanup = client(&url)
            .remove_playlist_duplicates("pl")
            .await
            .expect("cleanup");
        assert_eq!(
            cleanup,
            PlaylistCleanup {
                removed: 0,
                hidden: 1
            }
        );
        assert_eq!(hits(&requests, "POST"), 0);
        assert_eq!(hits(&requests, "DELETE"), 0);
    }

    #[test]
    fn hidden_entries_are_the_stored_items_the_listing_lacks() {
        let listed = [
            "0123456789abcdef0123456789abcdef",
            "ffffffffffffffffffffffffffffffff",
        ]
        .map(String::from);
        // The same items, spelled with dashes and capitals.
        let stored = [
            "01234567-89AB-CDEF-0123-456789ABCDEF",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
            "01234567-89ab-cdef-0123-456789abcdef",
        ]
        .map(String::from);
        assert_eq!(hidden_entries(&stored, &listed), 0);
        let with_hidden = [stored[0].clone(), "a".repeat(32), "a".repeat(32)];
        assert_eq!(hidden_entries(&with_hidden, &listed), 2);
    }

    #[test]
    fn only_guids_pass_as_item_ids() {
        assert!(is_item_id("0123456789abcdef0123456789ABCDEF"));
        assert!(is_item_id("01234567-89ab-cdef-0123-456789abcdef"));
        for bad in [
            "",
            "rock",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef-0123456789abcdef",
            "01234567-89ab-cdef-0123-456789abcdef,0",
            "012345678-9ab-cdef-0123-456789abcdef",
        ] {
            assert!(!is_item_id(bad), "{bad}");
        }
    }
}

#[cfg(test)]
mod stream_url_tests {
    use super::*;

    #[test]
    fn a_stream_is_asked_for_in_stereo_at_most() {
        let client = JellyfinClient::new("http://jelly.test:8096", "test-device", Vec::new())
            .expect("client")
            .with_session("session".into(), "user".into());
        let url = client.stream_url("track").expect("signed in");
        // The direct-play condition and the channel count of a transcode.
        assert!(url.contains("&maxAudioChannels=2"), "{url}");
        assert!(url.contains("&transcodingAudioChannels=2"), "{url}");
        // What belongs to one attempt at playing it is added when it opens.
        assert!(!url.contains("playSessionId"), "{url}");
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
        use SessionValidity::Valid;
        assert_eq!(results, (Valid, Valid, Valid, Valid));
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // A 401 newer than the last probe is not answered by it.
        assert_eq!(client.validate_after(Instant::now()).await, Valid);
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
/// SONARUNE_TEST_URL=https://jellyfin.example.com \
/// SONARUNE_TEST_USER=alice SONARUNE_TEST_PASS=secret \
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
    #[ignore = "needs a live server; set SONARUNE_TEST_{URL,USER,PASS}"]
    async fn live_flows() {
        let (Ok(url), Ok(user), Ok(pass)) = (
            std::env::var("SONARUNE_TEST_URL"),
            std::env::var("SONARUNE_TEST_USER"),
            std::env::var("SONARUNE_TEST_PASS"),
        ) else {
            eprintln!("skipping: set SONARUNE_TEST_URL / _USER / _PASS to run");
            return;
        };

        let mut client =
            JellyfinClient::new(url.trim_end_matches('/'), "sonarune-smoke", Vec::new())
                .expect("client");
        client.authenticate(&user, &pass).await.expect("login");
        eprintln!("✓ authenticated");
        // Shared like the app shares it (some calls fan out over tasks).
        let client = Arc::new(client);

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
            // The scan behind genre Play / Materialize must find the tracks
            // the server filtered for the same genre.
            let genre_tracks = client
                .smart_tracks(&SmartFilter {
                    genre_ids: vec![g.id.clone()],
                    ..SmartFilter::default()
                })
                .await
                .expect("smart_tracks (genre)");
            assert!(
                ga.items.is_empty() || !genre_tracks.is_empty(),
                "a genre with albums has tracks"
            );
            eprintln!("✓ genre '{}' scan: {} tracks", g.name, genre_tracks.len());
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
            .long_not_heard_albums(0, 12, true)
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
        assert!(
            collections.items.iter().all(|c| c.album_count > 0),
            "only collections with music albums are listed"
        );
        if let Some(collection) = collections.items.first() {
            let detail = client.collection(&collection.id).await.expect("collection");
            let albums = client
                .collection_albums(&collection.id)
                .await
                .expect("collection_albums");
            assert_eq!(
                collection.album_count as usize,
                albums.len(),
                "the listed album count is the collection's albums"
            );
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
                    .create_playlist("Sonarune smoke test — delete me", &seed[..1])
                    .await
                    .expect("create_playlist");
                eprintln!("✓ created playlist {pid}");
                if seed.len() >= 2 {
                    client
                        .playlist_add_chunk(&pid, &seed[1..2])
                        .await
                        .expect("playlist_add");
                }
                // 10.11 skips a track the playlist already holds and still
                // answers 204; the count is how an add tells what it did.
                let size = client.playlist(&pid).await.expect("playlist").track_count;
                client
                    .playlist_add_chunk(&pid, &seed[..1])
                    .await
                    .expect("playlist_add (already there)");
                assert_eq!(
                    client.playlist(&pid).await.expect("playlist").track_count,
                    size,
                    "a track the playlist holds is not added again"
                );
                // GET /Playlists/{id} names every stored entry: with nothing
                // hidden, exactly the items the listing shows.
                let stored = client
                    .playlist_stored_item_ids(&pid)
                    .await
                    .expect("playlist_stored_item_ids");
                let listed = client
                    .playlist_entry_item_ids(&pid)
                    .await
                    .expect("playlist_entry_item_ids");
                assert_eq!(stored.len(), listed.len(), "every entry is stored once");
                assert_eq!(hidden_entries(&stored, &listed), 0, "nothing is hidden");
                assert_eq!(
                    client
                        .remove_playlist_duplicates(&pid)
                        .await
                        .expect("remove_playlist_duplicates"),
                    PlaylistCleanup::default(),
                    "nothing to clean up"
                );
                let before = client.playlist_tracks(&pid).await.expect("playlist_tracks");
                let n_before = before.len();

                // The key contract to verify: a Name-only rename must NOT drop
                // the tracks (older UpdatePlaylistDto implementations did).
                client
                    .rename_playlist(&pid, "Sonarune smoke test — renamed")
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
                    renamed.name, "Sonarune smoke test — renamed",
                    "name updated"
                );
                eprintln!("✓ rename preserved {n_before} tracks");

                if after.len() >= 2 {
                    client
                        .playlist_move_audio(&pid, &after[0].entry_id, 1)
                        .await
                        .expect("playlist_move_audio");
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
