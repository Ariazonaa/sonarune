use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

// --- Wire types (Jellyfin JSON, PascalCase) ---

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResult {
    pub user: UserDto,
    pub access_token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub policy: UserPolicy,
}

/// Subset of the Jellyfin user policy we act on. Content deletion is granted
/// to admins, to users with the global flag, or to users allowed to delete
/// from specific folders.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserPolicy {
    pub is_administrator: bool,
    pub enable_content_deletion: bool,
    pub enable_content_deletion_from_folders: Vec<String>,
}

impl UserDto {
    /// Whether this user may permanently delete media on the server.
    pub fn can_delete(&self) -> bool {
        self.policy.is_administrator
            || self.policy.enable_content_deletion
            || !self.policy.enable_content_deletion_from_folders.is_empty()
    }

    /// Whether this user may edit item metadata. `POST /Items/{id}` is gated on
    /// Jellyfin's RequiresElevation policy — administrators only (there is no
    /// granular "edit metadata" user flag).
    pub fn can_edit_metadata(&self) -> bool {
        self.policy.is_administrator
    }
}

// --- Metadata editing (server-side, admin-only, whitelisted round-trip) ---
//
// `apply_metadata_edits` writes only six keys (Name, AlbumArtists,
// ProductionYear, Genres, IndexNumber, ParentIndexNumber) and leaves every other
// field of the fetched item untouched, so unknown/unsupported fields are never
// dropped on the round-trip. The `no_key_outside_the_whitelist_is_modified` test
// pins that invariant.

/// Desired final values for the in-scope fields (camelCase from the editor).
/// The editor always sends all six; an unchanged field simply re-writes the
/// same value. `None` on a numeric field clears it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataEdits {
    pub name: String,
    pub album_artists: Vec<String>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
}

/// Typed projection of the raw server item for the editor form + diff preview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditableMetadata {
    pub id: String,
    pub name: String,
    pub album_artists: Vec<String>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    /// `Type == "Audio"`: the editor shows Track/Disc fields (albums hide them).
    pub is_audio: bool,
}

impl EditableMetadata {
    pub fn from_raw(v: &Value) -> Self {
        Self {
            id: str_field(v, "Id"),
            name: str_field(v, "Name"),
            album_artists: name_list(v, "AlbumArtists"),
            year: int_field(v, "ProductionYear"),
            genres: str_list(v, "Genres"),
            track_number: int_field(v, "IndexNumber"),
            disc_number: int_field(v, "ParentIndexNumber"),
            is_audio: v.get("Type").and_then(Value::as_str) == Some("Audio"),
        }
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
fn int_field(v: &Value, key: &str) -> Option<i32> {
    v.get(key).and_then(Value::as_i64).map(|n| n as i32)
}
fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
/// AlbumArtists is an array of `{ "Name": …, "Id": … }`; read only the names.
fn name_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("Name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Trim, drop empties, and dedupe case-insensitively (matching the server's
/// `Distinct(OrdinalIgnoreCase)`), preserving order.
fn clean_names(list: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    list.iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_ascii_lowercase()))
        .map(str::to_string)
        .collect()
}

/// Merge whitelisted edits onto a full fetched item, in place. Only the six
/// whitelisted keys are ever written; every other field is left intact, so a
/// concurrent change to another field and any unknown fields are preserved.
pub fn apply_metadata_edits(item: &mut Value, edits: &MetadataEdits) -> AppResult<()> {
    let obj = item
        .as_object_mut()
        .ok_or_else(|| AppError::Other("server item is not a JSON object".into()))?;

    let name = edits.name.trim();
    if name.is_empty() {
        return Err(AppError::Other("title must not be empty".into()));
    }
    obj.insert("Name".into(), json!(name));

    let artists = clean_names(&edits.album_artists);
    obj.insert(
        "AlbumArtists".into(),
        Value::Array(artists.iter().map(|n| json!({ "Name": n })).collect()),
    );

    // The server rebuilds GenreItems from Genres on write; leave GenreItems be.
    obj.insert("Genres".into(), json!(clean_names(&edits.genres)));

    match edits.year {
        Some(y) if (1..=9999).contains(&y) => {
            obj.insert("ProductionYear".into(), json!(y));
        }
        Some(_) => return Err(AppError::Other("year must be between 1 and 9999".into())),
        None => {
            obj.insert("ProductionYear".into(), Value::Null);
        }
    }

    set_opt_index(obj, "IndexNumber", edits.track_number)?;
    set_opt_index(obj, "ParentIndexNumber", edits.disc_number)?;
    Ok(())
}

fn set_opt_index(
    obj: &mut serde_json::Map<String, Value>,
    key: &str,
    n: Option<i32>,
) -> AppResult<()> {
    match n {
        Some(v) if v < 0 => Err(AppError::Other(format!("{key} must not be negative"))),
        Some(v) => {
            obj.insert(key.into(), json!(v));
            Ok(())
        }
        None => {
            obj.insert(key.into(), Value::Null);
            Ok(())
        }
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "Id": "abc",
            "Name": "Old Title",
            "Type": "Audio",
            "ProductionYear": 1999,
            "AlbumArtists": [{ "Name": "Old Artist", "Id": "a1" }],
            "Genres": ["Rock"],
            "GenreItems": [{ "Name": "Rock", "Id": "g1" }],
            "IndexNumber": 3,
            "ParentIndexNumber": 1,
            "ProviderIds": { "MusicBrainzTrack": "mb-123" },
            "RunTimeTicks": 1234567_i64
        })
    }

    fn edits() -> MetadataEdits {
        MetadataEdits {
            name: "New Title".into(),
            album_artists: vec!["Artist A".into(), "Artist B".into()],
            year: Some(2020),
            genres: vec!["Pop".into(), "Electronic".into()],
            track_number: Some(5),
            disc_number: Some(2),
        }
    }

    #[test]
    fn only_whitelisted_keys_change_and_unknown_fields_survive() {
        let mut item = sample();
        apply_metadata_edits(&mut item, &edits()).unwrap();
        // Untouched fields preserved verbatim.
        assert_eq!(item["ProviderIds"]["MusicBrainzTrack"], json!("mb-123"));
        assert_eq!(item["RunTimeTicks"], json!(1234567_i64));
        assert_eq!(item["Id"], json!("abc"));
        assert_eq!(item["Type"], json!("Audio"));
        // Whitelisted fields updated.
        assert_eq!(item["Name"], json!("New Title"));
        assert_eq!(item["ProductionYear"], json!(2020));
        assert_eq!(item["IndexNumber"], json!(5));
        assert_eq!(item["ParentIndexNumber"], json!(2));
    }

    #[test]
    fn genres_are_written_as_a_string_array() {
        let mut item = sample();
        apply_metadata_edits(&mut item, &edits()).unwrap();
        assert_eq!(item["Genres"], json!(["Pop", "Electronic"]));
    }

    #[test]
    fn album_artists_become_name_objects() {
        let mut item = sample();
        apply_metadata_edits(&mut item, &edits()).unwrap();
        assert_eq!(
            item["AlbumArtists"],
            json!([{ "Name": "Artist A" }, { "Name": "Artist B" }])
        );
    }

    #[test]
    fn empty_title_is_rejected() {
        let mut item = sample();
        let mut e = edits();
        e.name = "   ".into();
        assert!(apply_metadata_edits(&mut item, &e).is_err());
    }

    #[test]
    fn clearing_year_writes_json_null() {
        let mut item = sample();
        let mut e = edits();
        e.year = None;
        apply_metadata_edits(&mut item, &e).unwrap();
        assert_eq!(item["ProductionYear"], Value::Null);
    }

    #[test]
    fn out_of_range_year_is_rejected() {
        let mut item = sample();
        let mut e = edits();
        e.year = Some(0);
        assert!(apply_metadata_edits(&mut item, &e).is_err());
    }

    #[test]
    fn negative_track_number_is_rejected() {
        let mut item = sample();
        let mut e = edits();
        e.track_number = Some(-1);
        assert!(apply_metadata_edits(&mut item, &e).is_err());
    }

    #[test]
    fn names_are_trimmed_and_deduped_case_insensitively() {
        let mut item = sample();
        let mut e = edits();
        e.genres = vec!["Rock".into(), " rock ".into(), String::new(), "Jazz".into()];
        apply_metadata_edits(&mut item, &e).unwrap();
        assert_eq!(item["Genres"], json!(["Rock", "Jazz"]));
    }

    #[test]
    fn projection_reads_all_fields() {
        let em = EditableMetadata::from_raw(&sample());
        assert_eq!(em.name, "Old Title");
        assert_eq!(em.album_artists, vec!["Old Artist"]);
        assert_eq!(em.year, Some(1999));
        assert_eq!(em.genres, vec!["Rock"]);
        assert_eq!(em.track_number, Some(3));
        assert_eq!(em.disc_number, Some(1));
        assert!(em.is_audio);
    }

    /// The safety-critical invariant: applying edits changes ONLY whitelisted
    /// keys and introduces no others, so a round-trip can never drop or corrupt
    /// unrelated server fields.
    #[test]
    fn no_key_outside_the_whitelist_is_modified() {
        const WHITELIST: &[&str] = &[
            "Name",
            "AlbumArtists",
            "ProductionYear",
            "Genres",
            "IndexNumber",
            "ParentIndexNumber",
        ];
        let before = sample();
        let mut after = before.clone();
        apply_metadata_edits(&mut after, &edits()).unwrap();
        let (b, a) = (before.as_object().unwrap(), after.as_object().unwrap());
        for (key, bval) in b {
            if a.get(key) != Some(bval) {
                assert!(
                    WHITELIST.contains(&key.as_str()),
                    "changed non-whitelisted `{key}`"
                );
            }
        }
        for key in a.keys() {
            assert!(
                b.contains_key(key) || WHITELIST.contains(&key.as_str()),
                "added non-whitelisted `{key}`"
            );
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct QuickConnectResult {
    pub secret: String,
    pub code: String,
    #[serde(default)]
    pub authenticated: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemsResponse {
    #[serde(default)]
    pub items: Vec<BaseItem>,
    #[serde(default)]
    pub total_record_count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct BaseItem {
    pub id: String,
    pub name: String,
    #[serde(rename = "Type")]
    pub item_type: String,
    pub album_artist: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub production_year: Option<i32>,
    pub index_number: Option<i32>,
    pub parent_index_number: Option<i32>,
    pub run_time_ticks: Option<i64>,
    pub image_tags: HashMap<String, String>,
    pub album_primary_image_tag: Option<String>,
    /// dB gain toward the server's loudness target for this track. Needs
    /// `fields=NormalizationGain` in the query.
    pub normalization_gain: Option<f32>,
    /// The same for the album the track belongs to — what album-mode
    /// normalization is supposed to use. Jellyfin only started sending it
    /// after 10.11 (jellyfin#14346), so it is usually absent and the player
    /// derives an album gain from the queue instead. Not requested through
    /// `fields`: the server fills it along with the album itself, and naming
    /// an unknown field would only get the whole request refused.
    #[serde(default)]
    pub album_normalization_gain: Option<f32>,
    /// ImageType -> (image tag -> blurhash). Included whenever images are.
    pub image_blur_hashes: HashMap<String, HashMap<String, String>>,
    /// Needs `enableUserData=true` in the query.
    pub user_data: Option<UserDataWire>,
    pub child_count: Option<i32>,
    /// Playlist entry id (only on /Playlists/{id}/Items responses); playlist
    /// removal/reorder address entries by this, not by item id.
    pub playlist_item_id: Option<String>,
    /// When the item was added to the library (needs `fields=DateCreated`).
    /// Used by the library-change watcher, not sent to the frontend.
    pub date_created: Option<String>,
    /// Biography/overview text (needs `fields=Overview`).
    pub overview: Option<String>,
    /// A track's artists as linkable items (id + name). Populated by default
    /// on audio DTOs — no `fields` flag needed.
    pub artist_items: Vec<NameGuidPair>,
    /// Linkable genres for Auto-DJ seeding (requires `fields=Genres`).
    pub genre_items: Vec<NameGuidPair>,
    /// An album's album-artists as linkable items.
    pub album_artists: Vec<NameGuidPair>,
}

/// Jellyfin `NameGuidPair` — an artist/genre reference with its item id.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct NameGuidPair {
    pub id: String,
    pub name: String,
}

impl BaseItem {
    /// Blurhash for the Primary image with the given tag (own or album's).
    fn primary_blur_hash(&self, tag: Option<&str>) -> Option<String> {
        self.image_blur_hashes.get("Primary")?.get(tag?).cloned()
    }
}

// --- DTOs sent to the frontend (camelCase) ---

/// A linkable artist reference (id + name) sent to the frontend so artist
/// names can navigate to `/artist/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenreRef {
    pub id: String,
    pub name: String,
}

/// Convert wire artist pairs to frontend refs, dropping entries without an id.
pub fn to_artist_refs(items: Vec<NameGuidPair>) -> Vec<ArtistRef> {
    items
        .into_iter()
        .filter(|a| !a.id.is_empty())
        .map(|a| ArtistRef {
            id: a.id,
            name: a.name,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistDto {
    pub id: String,
    pub name: String,
    pub image_tag: Option<String>,
    pub image_blur_hash: Option<String>,
    /// Biography/overview text (needs `fields=Overview`).
    pub overview: Option<String>,
}

impl ArtistDto {
    pub fn from_item(item: BaseItem) -> Self {
        let image_tag = item.image_tags.get("Primary").cloned();
        let image_blur_hash = item.primary_blur_hash(image_tag.as_deref());
        Self {
            id: item.id,
            name: item.name,
            image_tag,
            image_blur_hash,
            overview: item.overview.filter(|s| !s.trim().is_empty()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDto {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub year: Option<i32>,
    pub image_tag: Option<String>,
    pub image_blur_hash: Option<String>,
    pub is_favorite: bool,
    /// Track count (needs `fields=ChildCount`); drives the singles/EP hint.
    pub track_count: Option<i32>,
    /// Album-artists as linkable refs (falls back to track artists).
    pub artists: Vec<ArtistRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDto {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<String>,
    pub index_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub duration_ms: u64,
    /// Item whose Primary image represents this track (the album, usually).
    pub image_item_id: Option<String>,
    pub image_tag: Option<String>,
    pub image_blur_hash: Option<String>,
    pub normalization_gain: Option<f32>,
    /// The album's gain, when the server knows one (see `ItemWire`).
    pub album_normalization_gain: Option<f32>,
    pub is_favorite: bool,
    /// Server-side play count for this track.
    pub play_count: i32,
    /// The track's artists as linkable refs.
    pub artists: Vec<ArtistRef>,
    /// Linkable genre refs used by Auto-DJ seed selection.
    pub genres: Vec<GenreRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
}

/// Read-only Jellyfin collection (`BaseItemKind::BoxSet`). Jellysic exposes
/// album membership but deliberately no collection mutation commands.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDto {
    pub id: String,
    pub name: String,
    pub album_count: i32,
    pub image_tag: Option<String>,
    pub image_blur_hash: Option<String>,
}

impl CollectionDto {
    pub fn from_item(item: BaseItem) -> Self {
        let image_tag = item.image_tags.get("Primary").cloned();
        let image_blur_hash = item.primary_blur_hash(image_tag.as_deref());
        Self {
            id: item.id,
            name: item.name,
            album_count: item.child_count.unwrap_or(0),
            image_tag,
            image_blur_hash,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlayedFilter {
    #[default]
    All,
    Played,
    Unplayed,
}

/// Dynamic, locally named smart-view query. Only the query is sent to Rust;
/// names and saved definitions stay local to the frontend. All play data is
/// evaluated on Audio items, never MusicAlbum items.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SmartFilter {
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub genre_ids: Vec<String>,
    pub played: PlayedFilter,
    pub favorite_only: bool,
    pub min_play_count: Option<i32>,
    /// Inclusive ISO calendar date (`YYYY-MM-DD`) matched against DateCreated.
    pub added_since: Option<String>,
}

/// .NET ticks (100 ns) to milliseconds.
pub fn ticks_to_ms(ticks: i64) -> u64 {
    (ticks / 10_000).max(0) as u64
}

impl AlbumDto {
    pub fn from_item(item: BaseItem) -> Self {
        let image_tag = item.image_tags.get("Primary").cloned();
        let image_blur_hash = item.primary_blur_hash(image_tag.as_deref());
        Self {
            id: item.id,
            name: item.name,
            artist: item
                .album_artist
                .or_else(|| item.artists.first().cloned())
                .unwrap_or_default(),
            year: item.production_year,
            image_tag,
            image_blur_hash,
            is_favorite: item.user_data.map(|u| u.is_favorite).unwrap_or(false),
            track_count: item.child_count,
            artists: {
                let refs = to_artist_refs(item.album_artists);
                if refs.is_empty() {
                    to_artist_refs(item.artist_items)
                } else {
                    refs
                }
            },
        }
    }
}

// --- Lyrics (GET /Audio/{itemId}/Lyrics) ---

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LyricsResponse {
    pub metadata: LyricsMetadata,
    pub lyrics: Vec<LyricLine>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LyricsMetadata {
    pub is_synced: Option<bool>,
    /// Global lyric offset in ticks.
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LyricLine {
    pub text: String,
    /// Line timestamp in ticks; absent for unsynced lyrics.
    pub start: Option<i64>,
    /// Word-level (karaoke) timing, when the source format provides it.
    pub cues: Option<Vec<LyricLineCue>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LyricLineCue {
    pub start: i64,
    /// Character offset into the line text where this cue begins.
    pub position: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsDto {
    pub synced: bool,
    pub offset_ms: i64,
    pub lines: Vec<LyricLineDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLineDto {
    pub start_ms: Option<u64>,
    pub text: String,
    pub cues: Vec<LyricCueDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricCueDto {
    pub start_ms: u64,
    pub position: Option<i32>,
}

impl LyricsDto {
    /// "Track has no lyrics" — also what a 404 from the server maps to.
    pub fn empty() -> Self {
        Self {
            synced: false,
            offset_ms: 0,
            lines: Vec::new(),
        }
    }

    pub fn from_wire(wire: LyricsResponse) -> Self {
        // Some sources omit IsSynced; timestamped lines mean synced.
        let synced = wire
            .metadata
            .is_synced
            .unwrap_or_else(|| wire.lyrics.iter().any(|l| l.start.is_some()));
        Self {
            synced,
            offset_ms: wire.metadata.offset.unwrap_or(0) / 10_000,
            lines: wire
                .lyrics
                .into_iter()
                .map(|line| LyricLineDto {
                    start_ms: line.start.map(ticks_to_ms),
                    text: line.text,
                    cues: line
                        .cues
                        .unwrap_or_default()
                        .into_iter()
                        .map(|cue| LyricCueDto {
                            start_ms: ticks_to_ms(cue.start),
                            position: cue.position,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl TrackDto {
    pub fn from_item(item: BaseItem) -> Self {
        // Prefer the track's own Primary image, fall back to the album cover.
        let own_tag = item.image_tags.get("Primary").cloned();
        let (image_item_id, image_tag) = if own_tag.is_some() {
            (Some(item.id.clone()), own_tag)
        } else {
            (item.album_id.clone(), item.album_primary_image_tag.clone())
        };
        let image_blur_hash = item.primary_blur_hash(image_tag.as_deref());
        Self {
            id: item.id,
            name: item.name,
            artist: if item.artists.is_empty() {
                item.album_artist.unwrap_or_default()
            } else {
                item.artists.join(", ")
            },
            album: item.album.unwrap_or_default(),
            album_id: item.album_id,
            index_number: item.index_number,
            disc_number: item.parent_index_number,
            duration_ms: item.run_time_ticks.map(ticks_to_ms).unwrap_or(0),
            image_item_id,
            image_tag,
            image_blur_hash,
            normalization_gain: item.normalization_gain,
            album_normalization_gain: item.album_normalization_gain,
            is_favorite: item
                .user_data
                .as_ref()
                .map(|u| u.is_favorite)
                .unwrap_or(false),
            play_count: item.user_data.as_ref().map(|u| u.play_count).unwrap_or(0),
            artists: to_artist_refs(item.artist_items),
            genres: item
                .genre_items
                .into_iter()
                .filter(|genre| !genre.id.is_empty())
                .map(|genre| GenreRef {
                    id: genre.id,
                    name: genre.name,
                })
                .collect(),
        }
    }
}

// --- Track technical info (fields=MediaSources) ---

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSourcesResponse {
    pub items: Vec<MediaSourcesItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSourcesItem {
    pub media_sources: Vec<MediaSourceWire>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSourceWire {
    pub container: Option<String>,
    pub size: Option<i64>,
    pub path: Option<String>,
    pub media_streams: Vec<MediaStreamWire>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaStreamWire {
    #[serde(rename = "Type")]
    pub stream_type: String,
    pub codec: Option<String>,
    pub bit_rate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i32>,
}

/// Technical details of a track's audio file, for the "Info" panel.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfoDto {
    pub codec: Option<String>,
    pub bitrate_kbps: Option<i64>,
    pub sample_rate_hz: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i32>,
    pub container: Option<String>,
    pub size_bytes: Option<i64>,
    pub path: Option<String>,
}

impl TrackInfoDto {
    pub fn from_source(source: MediaSourceWire) -> Self {
        let audio = source
            .media_streams
            .into_iter()
            .find(|s| s.stream_type == "Audio")
            .unwrap_or_default();
        Self {
            codec: audio.codec,
            bitrate_kbps: audio.bit_rate.map(|b| b / 1000),
            sample_rate_hz: audio.sample_rate,
            bit_depth: audio.bit_depth,
            channels: audio.channels,
            container: source.container,
            size_bytes: source.size,
            path: source.path,
        }
    }
}

// --- Library slice: user data, playlists ---

/// Per-user item data (favorites, play counts). PascalCase wire type.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserDataWire {
    pub is_favorite: bool,
    pub play_count: i32,
    /// When the user last played the item (ISO-8601 UTC, as the server
    /// writes it). Playback sets it on tracks only, never on their album.
    pub last_played_date: Option<String>,
}

/// POST /Playlists response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CreatePlaylistResult {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDto {
    pub id: String,
    pub name: String,
    pub track_count: i32,
    pub image_tag: Option<String>,
    pub image_blur_hash: Option<String>,
}

impl PlaylistDto {
    pub fn from_item(item: BaseItem) -> Self {
        let image_tag = item.image_tags.get("Primary").cloned();
        let image_blur_hash = item.primary_blur_hash(image_tag.as_deref());
        Self {
            id: item.id,
            name: item.name,
            track_count: item.child_count.unwrap_or(0),
            image_tag,
            image_blur_hash,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistTrackDto {
    pub track: TrackDto,
    /// Playlist entry id — removal and reorder use this, not the item id.
    pub entry_id: String,
}

impl PlaylistTrackDto {
    pub fn from_item(item: BaseItem) -> Self {
        let entry_id = item.playlist_item_id.clone().unwrap_or_default();
        Self {
            track: TrackDto::from_item(item),
            entry_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenreDto {
    pub id: String,
    pub name: String,
}

impl GenreDto {
    pub fn from_item(item: BaseItem) -> Self {
        Self {
            id: item.id,
            name: item.name,
        }
    }
}
