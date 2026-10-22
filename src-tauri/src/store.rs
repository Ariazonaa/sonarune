use crate::error::AppResult;
use crate::identity::Server;
use rusqlite::{named_params, Connection};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// Small key/value store backed by SQLite in the app data dir.
/// Holds server config, device id, volume, and the persisted queue snapshot.
pub struct Store {
    conn: Mutex<Connection>,
    /// Serializes read-modify-write of the pinned-certificate map. `get` and
    /// `set` take `conn` separately, so two writers could lose an update (a
    /// forgotten pin coming back).
    trust: Mutex<()>,
    /// Bumped on every analysis write, so the sonic index knows when to
    /// rebuild (`sonic::index`).
    analysis_generation: std::sync::atomic::AtomicU64,
}

pub(crate) const DB_FILE: &str = "sonarune.db";

pub mod keys {
    pub const SERVER_URL: &str = "server_url";
    pub const USERNAME: &str = "username";
    pub const USER_ID: &str = "user_id";
    pub const DEVICE_ID: &str = "device_id";
    pub const ACCEPT_INVALID_CERTS: &str = "accept_invalid_certs";
    pub const TRUSTED_CERTS: &str = "trusted_certs";
    pub const VOLUME: &str = "volume";
    pub const QUEUE: &str = "queue";
    /// The queue index, saved apart from the track list in `QUEUE`.
    pub const QUEUE_INDEX: &str = "queue_index";
    pub const AUDIO_DSP: &str = "audio_dsp";
    pub const PLAYBACK: &str = "playback";
    pub const EXTRAS: &str = "extras";
    pub const PLAY_MODE: &str = "play_mode";
    pub const CAN_DELETE: &str = "can_delete";
    pub const CAN_EDIT: &str = "can_edit";
    pub const DESKTOP: &str = "desktop";
    pub const AUDIO_PROFILES: &str = "audio_profiles";
    /// The `AUDIO_PROFILES` value as it was when nothing in it could be read,
    /// set aside before a profile is saved over it (`audio_profiles.rs`).
    pub const AUDIO_PROFILES_UNREADABLE: &str = "audio_profiles_unreadable";
    pub const PLAYBACK_SPEED: &str = "playback_speed";
    /// Analyse the whole library in the background (`sonic_scan.rs`).
    pub const SONIC_SCAN: &str = "sonic_scan";
    /// How far the last complete listing found the analysis, for whom
    /// (`sonic_scan.rs`), so Settings has a number before the first listing
    /// of a session and while the scan is off.
    pub const SONIC_SCAN_PROGRESS: &str = "sonic_scan_progress";
}

/// `server_url` of an analysis whose track the last complete scan of its
/// server did not list: deleted, moved to a new item id, or hidden from the
/// account that scanned. It stays a while ([`RELEASED_KEPT`]) — another
/// account may still see the track, and a scan that lists it again claims
/// it back without a download — but no server's index offers it meanwhile.
const RELEASED: &str = "";

/// How long a released analysis is kept before it is deleted: a month for
/// another account's listing, or the track coming back, to claim it without
/// a download. A library that keeps changing does not pile up dead rows past
/// that.
pub(crate) const RELEASED_KEPT: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Which server answered at which address: when first and when last
/// (`identity.rs`). Reads no other table, so creating it cannot fail on a
/// damaged page elsewhere — what has to be there for the store to work.
const SERVER_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS server_address (
        address    TEXT    NOT NULL,
        server_id  TEXT    NOT NULL,
        first_seen INTEGER NOT NULL,
        last_seen  INTEGER NOT NULL,
        PRIMARY KEY (address, server_id)
    );";

/// The indexes the queries by server go through, so that neither the start
/// nor a sign-in reads every analysis or every play. Building one reads the
/// whole table, which a damaged page can fail: they are made where a
/// failure costs only speed (`Store::prepare`), never on the way to opening
/// the store.
const INDEXES: [&str; 5] = [
    "CREATE INDEX IF NOT EXISTS track_analysis_server
         ON track_analysis (server_url)",
    "CREATE INDEX IF NOT EXISTS track_analysis_owner
         ON track_analysis (server_id, server_url)",
    "CREATE INDEX IF NOT EXISTS track_analysis_released
         ON track_analysis (released_at) WHERE released_at IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS play_history_before_ids
         ON play_history (server_url, user_id) WHERE server_id IS NULL",
    "CREATE INDEX IF NOT EXISTS play_history_before_ids_by_user
         ON play_history (user_id, server_url) WHERE server_id IS NULL",
];

/// `Store::tidy`, in turn: an id next to an address it was never seen at
/// (looked at through the (id, address) index alone); a release date on a
/// row claimed back; none on a released row; and what was released long ago.
const TIDY_STALE_IDS: &str = "UPDATE track_analysis SET server_id = NULL
     WHERE rowid IN (
         SELECT t.rowid FROM track_analysis t
         WHERE t.server_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM server_address a
                           WHERE a.address = t.server_url AND a.server_id = t.server_id))";
const TIDY_CLAIMED_BACK: &str = "UPDATE track_analysis SET released_at = NULL
     WHERE released_at IS NOT NULL AND server_url IS NOT ''";
const TIDY_UNDATED: &str = "UPDATE track_analysis SET released_at = ?1
     WHERE server_url = '' AND released_at IS NULL";
const PURGE_RELEASED: &str = "DELETE FROM track_analysis
     WHERE released_at IS NOT NULL AND released_at < ?1 AND server_url = ''";
/// `Store::adopt_analyses`: the server's own to its address now, and the
/// ones stored before ids under an address it is given.
const ADOPT_TAGGED: &str = "UPDATE track_analysis SET server_url = :address
     WHERE server_id = :id AND server_url IS NOT :address AND server_url <> ''";
const ADOPT_FROM: &str = "UPDATE track_analysis SET server_url = :address, server_id = :id
     WHERE server_url = :from AND server_id IS NULL";

/// The addresses where the server `:id` is the one seen last.
pub(crate) const ADDRESSES_OF: &str = "
    SELECT a.address FROM server_address a
    WHERE a.server_id = :id
      AND NOT EXISTS (SELECT 1 FROM server_address b
                      WHERE b.address = a.address AND b.last_seen > a.last_seen)";

/// The analyses of the server (`:address`, `:id`): tagged with its id — or,
/// stored before its id was known or for a server whose id is not known,
/// with its address. Released rows and rows from before analyses recorded
/// their server (`server_url IS NULL`) are no server's: nothing says which
/// library such a track is in until a listing claims it.
const OWNED: &str = "server_url <> ''
     AND ((:id IS NOT NULL AND server_id = :id)
          OR ((server_id IS NULL OR :id IS NULL) AND server_url = :address))";

/// A stored analysis and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisRow {
    pub json: String,
    /// The address it was made or claimed under; `None` from before analyses
    /// recorded one, [`RELEASED`] once released.
    pub server_url: Option<String>,
    /// The Jellyfin id of its server, when that was known.
    pub server_id: Option<String>,
}

impl AnalysisRow {
    /// Tagged as `server`'s exactly, nothing left to claim.
    pub fn is_tagged_for(&self, server: &Server) -> bool {
        if self.server_url.as_deref() != Some(server.address.as_str()) {
            return false;
        }
        // A server whose id is not known goes by the address alone.
        server.id.is_none() || self.server_id == server.id
    }
}

/// Add `column` to `table` unless it is there. A column added after its
/// table was released is nullable: a build from before it does not name it,
/// so its writes leave it empty and its reads never see it.
fn add_column(conn: &Connection, table: &str, column: &str, kind: &str) -> rusqlite::Result<()> {
    let exists = conn
        .prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")?
        .exists([table, column])?;
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
    }
    Ok(())
}

/// Delete what was released longer than [`RELEASED_KEPT`] before `now`.
fn purge_released(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    let cutoff = now.saturating_sub(RELEASED_KEPT.as_millis() as i64);
    conn.execute(PURGE_RELEASED, [cutoff])
}

impl Store {
    pub fn open(dir: &Path) -> AppResult<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::error::AppError::Other(format!("cannot create data dir: {e}")))?;
        crate::legacy::migrate_db_file(dir, DB_FILE);
        let conn = Connection::open(dir.join(DB_FILE))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS kv (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS track_analysis (
                 item_id TEXT PRIMARY KEY,
                 json    TEXT NOT NULL
             );",
        )?;
        conn.execute_batch(crate::history::SCHEMA)?;
        // Whose an analysis is — the sonic index must only offer tracks of
        // the library signed in now: the address it was made under, the
        // Jellyfin id of its server, which stays when the address changes,
        // and when it was released. And the server a play was heard on.
        add_column(&conn, "track_analysis", "server_url", "TEXT")?;
        add_column(&conn, "track_analysis", "server_id", "TEXT")?;
        add_column(&conn, "track_analysis", "released_at", "INTEGER")?;
        add_column(&conn, "play_history", "server_id", "TEXT")?;
        conn.execute_batch(SERVER_SCHEMA)?;
        let store = Self {
            conn: Mutex::new(conn),
            trust: Mutex::new(()),
            analysis_generation: std::sync::atomic::AtomicU64::new(0),
        };
        store.prepare(now_ms_i64());
        Ok(store)
    }

    /// What reads whole tables at the start: the indexes, then [`tidy`].
    /// Not worth failing the start over — 0.6.1 started on a database with
    /// a damaged page in it, and failing here would have the whole database
    /// set aside (`db_recovery`), history and all. The store works without
    /// either, only slower or untidy.
    fn prepare(&self, now: i64) {
        // One by one: an index that cannot be built costs only itself.
        for index in INDEXES {
            if let Err(e) = self.with_conn(|conn| conn.execute_batch(index)) {
                tracing::warn!("cannot index the stored analyses and plays: {e}");
            }
        }
        if let Err(e) = self.tidy(now) {
            tracing::warn!("cannot tidy the stored analyses: {e}");
        }
    }

    /// Set straight what a build from before server ids may have left at
    /// odds with them, then delete what was released longer than
    /// [`RELEASED_KEPT`] before `now`. Such a build moves analyses by
    /// address alone — its claim, its release, a new analysis of a track
    /// already stored — and can leave an id next to an address that id was
    /// never seen at. There the id says nothing any more and the address
    /// decides again (`OWNED`), until the next sign-in settles the row
    /// (`identity::settle`). A row it released has no release date yet; one
    /// it claimed back still has one.
    ///
    /// Any row with an id can have been moved that way, so every one is
    /// looked at — but through the index of (id, address), never the rows
    /// with their analyses in them, and the release dates through their own
    /// partial index (`INDEXES`).
    fn tidy(&self, now: i64) -> AppResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(TIDY_STALE_IDS, [])?;
        tx.execute(TIDY_CLAIMED_BACK, [])?;
        tx.execute(TIDY_UNDATED, [now])?;
        purge_released(&tx, now)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .ok()
    }

    pub fn set(&self, key: &str, value: &str) -> AppResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// The server `id` answered at `address` just now. It becomes the one
    /// seen last there, and this its last sighting anywhere, even past a
    /// clock that went back: the order of sightings is what tells a server
    /// that moved from one that was replaced (`replaced_ids`). It answers
    /// there for the first time — again, when another server answered there
    /// since it last did.
    pub fn record_server(&self, address: &str, id: &str) -> AppResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO server_address (address, server_id, first_seen, last_seen)
             SELECT ?1, ?2, seen, seen FROM (
                 SELECT MAX(?3, COALESCE((SELECT MAX(last_seen) FROM server_address
                                          WHERE address = ?1 OR server_id = ?2), 0) + 1)
                        AS seen)
             WHERE true
             ON CONFLICT(address, server_id) DO UPDATE SET
                 first_seen = CASE
                     WHEN (SELECT server_id FROM server_address
                           WHERE address = ?1 ORDER BY last_seen DESC LIMIT 1) <> ?2
                     THEN excluded.first_seen
                     ELSE first_seen
                 END,
                 last_seen = excluded.last_seen",
            rusqlite::params![address, id, now_ms_i64()],
        )?;
        Ok(())
    }

    /// The id of the server seen last at `address`, if one ever answered
    /// there.
    pub fn server_id_at(&self, address: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT server_id FROM server_address WHERE address = ?1
             ORDER BY last_seen DESC LIMIT 1",
            [address],
            |row| row.get(0),
        )
        .ok()
    }

    /// The addresses where `id` is the server seen last.
    pub fn addresses_of(&self, id: &str) -> AppResult<Vec<String>> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(ADDRESSES_OF)?;
            let addresses = statement
                .query_map(named_params! { ":id": id }, |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(addresses)
        })
    }

    /// Ids `server` may have replaced at its address: last seen anywhere at
    /// that address, before `server` first answered there, and nowhere
    /// since. A server whose data was restored into a fresh install answers
    /// with a new id; one that only moved away answers somewhere else again.
    /// Which it is, `identity::settle` asks the account's history.
    pub fn replaced_ids(&self, server: &Server) -> AppResult<Vec<String>> {
        let Some(id) = &server.id else {
            return Ok(Vec::new());
        };
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT o.server_id FROM server_address o
                 WHERE o.address = :address AND o.server_id <> :id
                   AND o.last_seen < (SELECT n.first_seen FROM server_address n
                                      WHERE n.address = :address AND n.server_id = :id)
                   AND NOT EXISTS (SELECT 1 FROM server_address x
                                   WHERE x.server_id = o.server_id
                                     AND x.last_seen > o.last_seen)",
            )?;
            let ids = statement
                .query_map(
                    named_params! { ":address": server.address, ":id": id },
                    |row| row.get(0),
                )?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(ids)
        })
    }

    /// Tempo/key analysis of a track (see player/analysis.rs), as JSON.
    pub fn get_analysis(&self, item_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT json FROM track_analysis WHERE item_id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .ok()
    }

    /// [`get_analysis`](Self::get_analysis), with whose it is.
    pub fn analysis_row(&self, item_id: &str) -> Option<AnalysisRow> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT json, server_url, server_id FROM track_analysis WHERE item_id = ?1",
            [item_id],
            |row| {
                Ok(AnalysisRow {
                    json: row.get(0)?,
                    server_url: row.get(1)?,
                    server_id: row.get(2)?,
                })
            },
        )
        .ok()
    }

    /// Store an analysis of audio that came from `server`. The caller names
    /// it, as it was when the job started: a download or a decode can still
    /// be finishing after a sign-in to another server, and what is signed in
    /// by the time it is stored says nothing about where the audio came
    /// from. `None` stores it as nobody's, the way analyses were stored
    /// before they recorded their server.
    pub fn put_analysis_from(
        &self,
        server: Option<&Server>,
        item_id: &str,
        json: &str,
    ) -> AppResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO track_analysis (item_id, json, server_url, server_id, released_at)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(item_id) DO UPDATE SET json = excluded.json,
                                                server_url = excluded.server_url,
                                                server_id = excluded.server_id,
                                                released_at = NULL",
            rusqlite::params![
                item_id,
                json,
                server.map(|s| s.address.as_str()),
                server.and_then(|s| s.id.as_deref())
            ],
        )?;
        self.analysis_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Tag these analyses as `server`'s: its library lists the tracks, so
    /// they belong to it, whatever they were tagged with — the same server
    /// under a new address keeps its index, and a released one comes back.
    /// Returns how many moved.
    pub fn claim_analyses(&self, server: &Server, item_ids: &[String]) -> AppResult<usize> {
        self.retag_analyses(
            "UPDATE track_analysis
             SET server_url = :address, server_id = :id, released_at = NULL
             WHERE item_id = :item AND (server_url IS NOT :address OR server_id IS NOT :id)",
            server,
            item_ids,
            |_| Ok(()),
        )
    }

    /// Take these analyses of `server` out of its index ([`RELEASED`]): a
    /// complete scan of its library did not list them. What was released
    /// longer than [`RELEASED_KEPT`] ago is deleted at the same time.
    /// Returns how many were released.
    pub fn release_analyses(&self, server: &Server, item_ids: &[String]) -> AppResult<usize> {
        let now = now_ms_i64();
        self.retag_analyses(
            &format!(
                "UPDATE track_analysis
                 SET server_url = '', server_id = NULL, released_at = {now}
                 WHERE item_id = :item AND {OWNED}"
            ),
            server,
            item_ids,
            |tx| purge_released(tx, now).map(|_| ()),
        )
    }

    /// Settle the analyses of `server` (`identity::settle`): with its id
    /// known, the ones tagged with it move to the address it is reached at
    /// now, which is what a build from before ids reads. The ones stored
    /// before ids were kept under one of `addresses` are given to it — with
    /// its id known, the id and its address now; without, its address, as
    /// before ids were kept. Released rows stay released, and a row that
    /// carries another server's id stays that server's. Every statement
    /// goes through an index: nothing is read when nothing is to move.
    /// Returns how many moved.
    pub fn adopt_analyses(&self, server: &Server, addresses: &[String]) -> AppResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut moved = 0;
        let (address, id) = (server.address.as_str(), server.id.as_deref());
        {
            if id.is_some() {
                moved += tx.execute(
                    ADOPT_TAGGED,
                    named_params! { ":address": address, ":id": id },
                )?;
            }
            let mut statement = tx.prepare(ADOPT_FROM)?;
            for from in addresses {
                // Its own address is its own already when the id is not known.
                if from == RELEASED || (id.is_none() && from == address) {
                    continue;
                }
                moved += statement
                    .execute(named_params! { ":address": address, ":id": id, ":from": from })?;
            }
        }
        tx.commit()?;
        if moved > 0 {
            self.analysis_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(moved)
    }

    /// Give `server` the analyses tagged with `old`: the id it replaced
    /// (`replaced_ids`). Returns how many.
    pub fn take_over_analyses(&self, old: &str, server: &Server) -> AppResult<usize> {
        let moved = self.with_conn(|conn| {
            conn.execute(
                "UPDATE track_analysis SET server_url = :address, server_id = :id
                 WHERE server_id = :old AND server_url <> ''",
                named_params! { ":address": server.address, ":id": server.id, ":old": old },
            )
        })?;
        if moved > 0 {
            self.analysis_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(moved)
    }

    /// Run `sql`, which names `:address`, `:id` and `:item`, for `server`
    /// and every id, then `finish` in the same transaction.
    fn retag_analyses(
        &self,
        sql: &str,
        server: &Server,
        item_ids: &[String],
        finish: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<()>,
    ) -> AppResult<usize> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        // One transaction for a page of a library, not a commit per track.
        let tx = conn.transaction()?;
        let mut changed = 0;
        {
            let mut statement = tx.prepare(sql)?;
            for item_id in item_ids {
                changed += statement.execute(named_params! {
                    ":address": server.address,
                    ":id": server.id,
                    ":item": item_id,
                })?;
            }
        }
        finish(&tx)?;
        tx.commit()?;
        if changed > 0 {
            self.analysis_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(changed)
    }

    /// Item ids of the analyses that are `server`'s (`OWNED`): what a
    /// complete listing of its library may release. That includes the ones
    /// stored before ids were kept under its address — one of a server that
    /// stood there before goes with them, and any listing claims it back
    /// within [`RELEASED_KEPT`]; kept out of reach of a release, a track
    /// deleted from the library would stay in the index for good.
    pub fn analysis_ids_of(&self, server: &Server) -> AppResult<Vec<String>> {
        self.with_conn(|conn| {
            let mut statement =
                conn.prepare(&format!("SELECT item_id FROM track_analysis WHERE {OWNED}"))?;
            let ids = statement
                .query_map(
                    named_params! { ":address": server.address, ":id": server.id },
                    |row| row.get(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ids)
        })
    }

    /// Every stored analysis that is `server`'s, as (item id, JSON).
    pub fn analyses_for(&self, server: &Server) -> AppResult<Vec<(String, String)>> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT item_id, json FROM track_analysis WHERE {OWNED}"
            ))?;
            let rows = statement
                .query_map(
                    named_params! { ":address": server.address, ":id": server.id },
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn analysis_generation(&self) -> u64 {
        self.analysis_generation
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Run `f` on the connection, for modules with tables of their own
    /// (`history`).
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> AppResult<T> {
        let conn = self.conn.lock().unwrap();
        Ok(f(&conn)?)
    }

    pub fn delete(&self, key: &str) -> AppResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
        Ok(())
    }

    /// All settings-like keys as one JSON object (settings export). The
    /// queue snapshot and identity keys are deliberately excluded; the
    /// ListenBrainz token is a credential and never leaves the machine.
    pub fn export_settings(&self) -> AppResult<String> {
        let mut map = serde_json::Map::new();
        for key in [
            keys::VOLUME,
            keys::AUDIO_DSP,
            keys::PLAYBACK,
            keys::EXTRAS,
            keys::DESKTOP,
            keys::AUDIO_PROFILES,
            keys::PLAYBACK_SPEED,
        ] {
            if let Some(value) = self.get(key) {
                let value = if key == keys::EXTRAS {
                    // Unreadable: left out rather than risk a credential, and
                    // rather than an empty blob that would reset every extra
                    // on import.
                    match strip_extras_token(&value) {
                        Some(stripped) => stripped,
                        None => continue,
                    }
                } else {
                    value
                };
                map.insert(key.to_string(), serde_json::Value::String(value));
            }
        }
        serde_json::to_string_pretty(&map)
            .map_err(|e| crate::error::AppError::Other(format!("export failed: {e}")))
    }

    pub fn import_settings(&self, json: &str) -> AppResult<Vec<String>> {
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json)
            .map_err(|e| crate::error::AppError::Other(format!("not a settings export: {e}")))?;
        let mut imported = Vec::new();
        for key in [
            keys::VOLUME,
            keys::AUDIO_DSP,
            keys::PLAYBACK,
            keys::EXTRAS,
            keys::DESKTOP,
            keys::AUDIO_PROFILES,
            keys::PLAYBACK_SPEED,
        ] {
            if let Some(serde_json::Value::String(value)) = map.get(key) {
                let value = match key {
                    // A hand-edited export must not poison the output gain.
                    keys::VOLUME => match value.parse::<f32>() {
                        Ok(v) if v.is_finite() => v.clamp(0.0, 1.0).to_string(),
                        _ => continue,
                    },
                    // Imports never carry a credential into the store. A blob
                    // that is not JSON at all is skipped like any other: the
                    // settings type fills in defaults for missing fields, so
                    // anything that stood in for it would reset every extra
                    // — history recording back on, Auto-DJ off.
                    keys::EXTRAS => {
                        let Some(stripped) = strip_extras_token(value) else {
                            tracing::warn!("settings import skipped an entry that does not parse");
                            continue;
                        };
                        if !parses_as::<crate::player::ExtrasSettings>(&stripped) {
                            continue;
                        }
                        stripped
                    }
                    // The remaining entries are nested JSON blobs. Writing one
                    // that does not deserialize would leave the store holding
                    // a value every later read silently falls back from — the
                    // user's EQ/playback/desktop configuration would be gone
                    // while the import still reported success.
                    keys::AUDIO_DSP => {
                        if !parses_as::<crate::player::dsp::DspParams>(value) {
                            continue;
                        }
                        value.clone()
                    }
                    keys::PLAYBACK => {
                        if !parses_as::<crate::player::PlaybackSettings>(value) {
                            continue;
                        }
                        value.clone()
                    }
                    keys::DESKTOP => {
                        if !parses_as::<crate::desktop::DesktopSettings>(value) {
                            continue;
                        }
                        value.clone()
                    }
                    keys::AUDIO_PROFILES => {
                        if !parses_as::<crate::audio_profiles::AudioProfiles>(value) {
                            continue;
                        }
                        value.clone()
                    }
                    // Clamped on the way in: a hand-edited 50x stays 2x.
                    keys::PLAYBACK_SPEED => {
                        match serde_json::from_str::<crate::player::tempo::SpeedSetting>(value) {
                            Ok(setting) => serde_json::to_string(&setting.clamped())
                                .unwrap_or_else(|_| value.clone()),
                            Err(_) => continue,
                        }
                    }
                    _ => value.clone(),
                };
                self.set(key, &value)?;
                imported.push(key.to_string());
            }
        }
        Ok(imported)
    }

    /// Pinned certificates as a JSON map `{ server_url: [entry, …] }`, so
    /// several servers keep independent trust. An entry is
    /// `{ "fingerprint": "AB:…", "pinnedAt": <unix ms> }`; installs from before
    /// dates were recorded hold a bare fingerprint string, which stays valid.
    fn trusted_map(&self) -> serde_json::Map<String, serde_json::Value> {
        self.get(keys::TRUSTED_CERTS)
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save_trusted_map(&self, map: serde_json::Map<String, serde_json::Value>) -> AppResult<()> {
        self.set(
            keys::TRUSTED_CERTS,
            &serde_json::Value::Object(map).to_string(),
        )
    }

    /// SHA-256 fingerprints the user has confirmed for `server_url`.
    pub fn trusted_fingerprints(&self, server_url: &str) -> Vec<String> {
        self.trusted_map()
            .get(server_url)
            .and_then(|v| v.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| pin_fingerprint_of(e).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every pinned certificate across all servers, for the settings list.
    pub fn trusted_certs(&self) -> Vec<TrustedCert> {
        let mut certs: Vec<TrustedCert> = self
            .trusted_map()
            .iter()
            .flat_map(|(server_url, entries)| {
                entries
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(move |entry| {
                        Some(TrustedCert {
                            server_url: server_url.clone(),
                            fingerprint: pin_fingerprint_of(entry)?.to_string(),
                            pinned_at: entry.get("pinnedAt").and_then(|v| v.as_u64()),
                        })
                    })
            })
            .collect();
        certs.sort_by(|a, b| a.server_url.cmp(&b.server_url));
        certs
    }

    /// Pin `fingerprint` as *the* trusted certificate for `server_url`,
    /// replacing whatever was pinned for it before.
    ///
    /// Replacing matters: a server that rotated its key, or a certificate
    /// someone confirmed by mistake, must not stay trusted next to the new one
    /// — whoever holds the old key would otherwise be accepted for good. Callers
    /// pin only a certificate the server actually presented on a successful
    /// login (or on first use with automatic trust).
    pub fn pin_fingerprint(&self, server_url: &str, fingerprint: &str) -> AppResult<()> {
        if !crate::tls::is_valid_fingerprint(fingerprint) {
            return Err(crate::error::AppError::Other(
                "not a SHA-256 certificate fingerprint".into(),
            ));
        }
        let _trust = self.trust.lock().unwrap();
        let mut map = self.trusted_map();
        // Re-pinning the certificate that is already pinned keeps its date.
        let pinned_at = map
            .get(server_url)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .find(|e| pin_fingerprint_of(e).is_some_and(|fp| crate::tls::matches(fp, fingerprint)))
            .and_then(|e| e.get("pinnedAt").and_then(|v| v.as_u64()))
            .or_else(now_ms);
        map.insert(
            server_url.to_string(),
            serde_json::json!([{ "fingerprint": fingerprint, "pinnedAt": pinned_at }]),
        );
        self.save_trusted_map(map)
    }

    /// Remove the pin for `fingerprint` on `server_url`. Returns whether one
    /// was removed. The next connection to that server has to be confirmed
    /// again (or is trusted on first use, when automatic trust is on).
    pub fn forget_fingerprint(&self, server_url: &str, fingerprint: &str) -> AppResult<bool> {
        let _trust = self.trust.lock().unwrap();
        let mut map = self.trusted_map();
        let Some(entries) = map.get_mut(server_url).and_then(|v| v.as_array_mut()) else {
            return Ok(false);
        };
        let before = entries.len();
        entries.retain(|e| {
            !pin_fingerprint_of(e).is_some_and(|fp| crate::tls::matches(fp, fingerprint))
        });
        let removed = entries.len() != before;
        if entries.is_empty() {
            map.remove(server_url);
        }
        if removed {
            self.save_trusted_map(map)?;
        }
        Ok(removed)
    }

    /// Record the "trust this server's certificate automatically" preference
    /// after a sign-in, restore or re-login went through. It only ever acts on
    /// a server with nothing pinned (`commands::autotrust`, trust on first
    /// use). A login that succeeded with nothing pinned proved a publicly
    /// valid certificate, so the preference is dropped: kept, a later silent
    /// re-login or restore would pin whatever an attacker presents instead.
    pub fn settle_auto_trust(&self, server_url: &str, requested: bool) -> AppResult<()> {
        let keep = requested && !self.trusted_fingerprints(server_url).is_empty();
        self.set(
            keys::ACCEPT_INVALID_CERTS,
            if keep { "true" } else { "false" },
        )
    }

    /// Stable per-installation device id, created on first use.
    pub fn device_id(&self) -> AppResult<String> {
        if let Some(id) = self.get(keys::DEVICE_ID) {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.set(keys::DEVICE_ID, &id)?;
        Ok(id)
    }
}

/// One pinned certificate, as the settings list shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedCert {
    pub server_url: String,
    pub fingerprint: String,
    /// Unix milliseconds; `None` for pins stored before dates were recorded.
    pub pinned_at: Option<u64>,
}

/// The fingerprint of a stored pin entry, in either the current object form
/// or the legacy bare-string form.
fn pin_fingerprint_of(entry: &serde_json::Value) -> Option<&str> {
    entry
        .as_str()
        .or_else(|| entry.get("fingerprint").and_then(|v| v.as_str()))
}

fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// Unix milliseconds, as the tables keep them.
fn now_ms_i64() -> i64 {
    now_ms().map_or(0, |ms| ms as i64)
}

/// Whether a stored blob still deserializes into the settings type that reads
/// it. Used to reject an import instead of writing a value that every later
/// read would silently discard.
fn parses_as<T: serde::de::DeserializeOwned>(raw: &str) -> bool {
    match serde_json::from_str::<T>(raw) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("settings import skipped an entry that does not parse: {e}");
            false
        }
    }
}

/// Remove the ListenBrainz token from a serialized extras blob. The token
/// lives in the Windows Credential Manager; the store/export only ever sees
/// the non-secret fields. `None` for a blob that is not JSON: there is no
/// telling what in it is a credential, so the caller drops all of it.
fn strip_extras_token(extras_json: &str) -> Option<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(extras_json).ok()?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("listenbrainzToken");
    }
    Some(value.to_string())
}

#[cfg(test)]
mod compat_tests;

#[cfg(test)]
mod tests {
    use super::{keys, Server, Store, RELEASED, RELEASED_KEPT};
    use std::path::PathBuf;

    /// A store in its own temp directory. The connection is closed before the
    /// directory is removed — Windows refuses to delete an open database file.
    struct TempStore {
        store: Option<Store>,
        dir: PathBuf,
    }

    impl std::ops::Deref for TempStore {
        type Target = Store;
        fn deref(&self) -> &Store {
            self.store.as_ref().expect("store is open until drop")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            self.store.take();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn address(address: &str) -> Server {
        Server {
            address: address.into(),
            id: None,
        }
    }

    fn server(address: &str, id: &str) -> Server {
        Server {
            address: address.into(),
            id: Some(id.into()),
        }
    }

    fn ids_of(store: &Store, server: &Server) -> Vec<String> {
        let mut ids: Vec<String> = store
            .analyses_for(server)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        ids
    }

    fn list(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn row(store: &Store, id: &str) -> (Option<String>, Option<String>) {
        let row = store.analysis_row(id).expect("stored");
        (row.server_url, row.server_id)
    }

    fn released_at(store: &Store, id: &str) -> Option<i64> {
        store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT released_at FROM track_analysis WHERE item_id = ?1",
                    [id],
                    |row| row.get(0),
                )
            })
            .unwrap()
    }

    #[test]
    fn analyses_are_listed_for_the_server_they_were_made_on() {
        let store = temp_store();
        let (a, b) = (address("https://a.home"), address("https://b.home"));
        store.put_analysis_from(Some(&a), "a1", "{}").unwrap();
        store.put_analysis_from(Some(&b), "b1", "{}").unwrap();
        assert_eq!(ids_of(&store, &a), ["a1"]);
        assert_eq!(ids_of(&store, &b), ["b1"]);
        // Re-analysed from b: it moves there.
        store.put_analysis_from(Some(&b), "a1", "{}").unwrap();
        assert!(ids_of(&store, &a).is_empty());
        // Stored by nobody — the way 0.6.0 stored every analysis: in no
        // server's index until a listing claims it.
        store.put_analysis_from(None, "legacy", "{}").unwrap();
        assert_eq!(ids_of(&store, &b), ["a1", "b1"]);
        assert_eq!(row(&store, "legacy"), (None, None));
    }

    #[test]
    fn an_analysis_is_filed_under_the_server_its_job_started_for() {
        // A download from server A that finishes after a sign-in to B: the
        // job names A, whatever the store says is signed in by then.
        let store = temp_store();
        let a = server("http://10.0.0.5:8096", "aaaa");
        store.record_server(&a.address, "aaaa").unwrap();
        store.set(keys::SERVER_URL, "https://b.home").unwrap();
        store.put_analysis_from(Some(&a), "t", "{}").unwrap();
        assert_eq!(
            row(&store, "t"),
            (Some(a.address.clone()), Some("aaaa".into()))
        );
        assert_eq!(ids_of(&store, &a), ["t"]);
        assert!(ids_of(&store, &address("https://b.home")).is_empty());
    }

    #[test]
    fn a_listed_track_is_claimed_and_an_unlisted_one_released() {
        let store = temp_store();
        let old = address("http://10.0.0.5:8096");
        let new = address("https://music.example.org");
        store.put_analysis_from(Some(&old), "t1", "{}").unwrap();
        store.put_analysis_from(Some(&old), "t2", "{}").unwrap();
        store.put_analysis_from(None, "legacy", "{}").unwrap();
        // The same server under a new address: what its library lists is
        // its own, without a download.
        let generation = store.analysis_generation();
        assert_eq!(
            store
                .claim_analyses(&new, &list(&["t1", "legacy"]))
                .unwrap(),
            2
        );
        assert!(
            store.analysis_generation() > generation,
            "the index rebuilds"
        );
        assert_eq!(ids_of(&store, &new), ["legacy", "t1"]);
        assert_eq!(ids_of(&store, &old), ["t2"]);
        assert_eq!(store.claim_analyses(&new, &list(&["t1"])).unwrap(), 0);
        // A complete scan did not list t1 any more: it leaves every index,
        // but its analysis stays.
        assert_eq!(store.release_analyses(&new, &list(&["t1"])).unwrap(), 1);
        assert_eq!(ids_of(&store, &new), ["legacy"]);
        assert!(!ids_of(&store, &old).contains(&"t1".to_string()));
        assert!(store.get_analysis("t1").is_some());
        assert!(released_at(&store, "t1").is_some());
        // Listed again: claimed back.
        assert_eq!(store.claim_analyses(&new, &list(&["t1"])).unwrap(), 1);
        assert_eq!(ids_of(&store, &new), ["legacy", "t1"]);
        assert_eq!(released_at(&store, "t1"), None);
        // A server only releases its own.
        assert_eq!(store.release_analyses(&new, &list(&["t2"])).unwrap(), 0);
        assert_eq!(store.analysis_ids_of(&old).unwrap(), ["t2"]);
    }

    #[test]
    fn a_server_keeps_its_analyses_under_every_address() {
        let store = temp_store();
        let (lan, proxy) = ("http://10.0.0.5:8096", "https://music.example.org");
        store.record_server(lan, "s1").unwrap();
        store
            .put_analysis_from(Some(&server(lan, "s1")), "a", "{}")
            .unwrap();
        // Reached through the proxy now: the index goes by the id, before
        // anything is moved.
        store.record_server(proxy, "s1").unwrap();
        let now = server(proxy, "s1");
        assert_eq!(ids_of(&store, &now), ["a"]);
        // Settled, the row names the address too, for a build that reads
        // only that.
        assert_eq!(store.adopt_analyses(&now, &[]).unwrap(), 1);
        assert_eq!(row(&store, "a"), (Some(proxy.into()), Some("s1".into())));
        assert_eq!(store.adopt_analyses(&now, &[]).unwrap(), 0, "once");
        // Neither the old address nor another server sees it.
        assert!(ids_of(&store, &address(lan)).is_empty());
        assert!(ids_of(&store, &server(proxy, "s2")).is_empty());
    }

    #[test]
    fn two_servers_at_one_address_keep_their_own_analyses() {
        // A copy of a server put up at the address of the first: same item
        // ids, same user ids, a server id of its own.
        let store = temp_store();
        let at = "http://10.0.0.9:8096";
        store.record_server(at, "first").unwrap();
        store
            .put_analysis_from(Some(&server(at, "first")), "mine", "{}")
            .unwrap();
        // From before ids were kept, under that address.
        store
            .put_analysis_from(Some(&address(at)), "older", "{}")
            .unwrap();
        store.record_server(at, "copy").unwrap();
        assert_eq!(store.server_id_at(at).as_deref(), Some("copy"));
        let copy = server(at, "copy");
        // By its address the older one counts for whoever stands there.
        assert_eq!(ids_of(&store, &copy), ["older"]);
        assert_eq!(store.adopt_analyses(&copy, &list(&[at])).unwrap(), 1);
        assert_eq!(row(&store, "mine"), (Some(at.into()), Some("first".into())));
        assert_eq!(ids_of(&store, &server(at, "first")), ["mine"]);
        // The copy releases only its own.
        assert_eq!(
            store
                .release_analyses(&copy, &list(&["mine", "older"]))
                .unwrap(),
            1
        );
        assert_eq!(ids_of(&store, &server(at, "first")), ["mine"]);
    }

    #[test]
    fn an_old_analysis_of_a_track_gone_from_the_library_is_released_too() {
        // Stored before ids under the server's address, and never given to
        // it: the account that played it there never signed in again. The
        // track is deleted; it must not stay in the index for good.
        let store = temp_store();
        let at = "https://music.home";
        store
            .put_analysis_from(Some(&address(at)), "deleted", "{}")
            .unwrap();
        store.record_server(at, "s1").unwrap();
        let me = server(at, "s1");
        assert_eq!(ids_of(&store, &me), ["deleted"]);
        assert_eq!(store.analysis_ids_of(&me).unwrap(), ["deleted"]);
        assert_eq!(store.release_analyses(&me, &list(&["deleted"])).unwrap(), 1);
        assert!(ids_of(&store, &me).is_empty());
        // Another server's listing that names it takes it back, within the
        // time released analyses are kept.
        let other = server("https://other.home", "s2");
        assert_eq!(
            store.claim_analyses(&other, &list(&["deleted"])).unwrap(),
            1
        );
        assert_eq!(ids_of(&store, &other), ["deleted"]);
    }

    #[test]
    fn analyses_from_before_server_ids_go_where_they_are_given() {
        // 0.6.1 tagged analyses by address only. Which addresses are a
        // server's is `identity::settle`'s to say.
        let store = temp_store();
        let (here, there, away) = (
            "https://music.example.org",
            "http://10.0.0.5:8096",
            "http://old.box:8096",
        );
        for (id, at) in [
            ("h", here),
            ("t", there),
            ("w", away),
            ("x", "https://x.home"),
        ] {
            store
                .put_analysis_from(Some(&address(at)), id, "{}")
                .unwrap();
        }
        store.put_analysis_from(None, "legacy", "{}").unwrap();
        store
            .put_analysis_from(Some(&address(RELEASED)), "released", "{}")
            .unwrap();
        store
            .put_analysis_from(Some(&server(away, "s2")), "theirs", "{}")
            .unwrap();
        store.record_server(there, "s1").unwrap();
        store.record_server(here, "s1").unwrap();
        assert_eq!(store.addresses_of("s1").unwrap().len(), 2);
        let me = server(here, "s1");
        assert_eq!(
            store
                .adopt_analyses(&me, &list(&[here, there, away, RELEASED]))
                .unwrap(),
            3
        );
        assert_eq!(ids_of(&store, &me), ["h", "t", "w"]);
        assert_eq!(row(&store, "x"), (Some("https://x.home".into()), None));
        assert_eq!(row(&store, "legacy"), (None, None));
        assert_eq!(row(&store, "released"), (Some(RELEASED.into()), None));
        assert_eq!(
            row(&store, "theirs"),
            (Some(away.into()), Some("s2".into()))
        );
        assert_eq!(store.adopt_analyses(&me, &list(&[here, away])).unwrap(), 0);
        // Without an id, they move to its address, as before ids were kept,
        // and only rows that carry no id.
        let unknown = address("https://new.home");
        assert_eq!(
            store
                .adopt_analyses(&unknown, &list(&["https://x.home", here]))
                .unwrap(),
            1
        );
        assert_eq!(ids_of(&store, &unknown), ["x"]);
        assert_eq!(ids_of(&store, &me), ["h", "t", "w"]);
    }

    #[test]
    fn the_start_and_a_sign_in_read_through_indexes_only() {
        // A full scan of the analyses reads every one of them, JSON and all:
        // tens of megabytes on a large library, at every start.
        let store = temp_store();
        let plan = |sql: &str| -> Vec<String> {
            store
                .with_conn(|conn| {
                    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                    // Parameters left unbound: only the plan is wanted.
                    let mut rows = statement.raw_query();
                    let mut details = Vec::new();
                    while let Some(row) = rows.next()? {
                        details.push(row.get::<_, String>(3)?);
                    }
                    Ok(details)
                })
                .unwrap()
        };
        for (sql, through) in [
            (super::TIDY_STALE_IDS, "COVERING INDEX track_analysis_owner"),
            (super::TIDY_CLAIMED_BACK, "track_analysis_released"),
            (super::TIDY_UNDATED, "track_analysis_server"),
            (super::PURGE_RELEASED, "INDEX track_analysis_"),
            (super::ADOPT_TAGGED, "track_analysis_owner"),
            (super::ADOPT_FROM, "INDEX track_analysis_"),
            (crate::history::ADOPT_PLAYS, "play_history_before_ids"),
            (
                crate::history::ADDRESSES_BEFORE_IDS,
                "play_history_before_ids",
            ),
            (crate::history::OTHERS_BEFORE_IDS, "play_history_before_ids"),
        ] {
            let plan = plan(sql);
            assert!(
                plan.iter().any(|step| step.contains(through)),
                "{plan:?}\n  in {sql}"
            );
            for step in &plan {
                // The small table of server addresses may be read whole.
                let table = step.split_whitespace().nth(1).unwrap_or_default();
                let scans_a_big_table = step.starts_with("SCAN")
                    && ["track_analysis", "play_history", "t"].contains(&table)
                    && !step.contains("INDEX");
                assert!(!scans_a_big_table, "{step}\n  in {sql}");
            }
        }
    }

    #[test]
    fn a_released_analysis_is_deleted_after_a_while() {
        let store = temp_store();
        let me = address("https://music.home");
        for id in ["old", "recent", "kept"] {
            store.put_analysis_from(Some(&me), id, "{}").unwrap();
        }
        store
            .release_analyses(&me, &list(&["old", "recent"]))
            .unwrap();
        let day = 24 * 60 * 60 * 1000;
        let kept = RELEASED_KEPT.as_millis() as i64;
        let now = super::now_ms_i64();
        store
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE track_analysis SET released_at = ?1 WHERE item_id = 'old'",
                    [now - kept - day],
                )
            })
            .unwrap();
        // Another release is when the old ones go.
        store.release_analyses(&me, &list(&["kept"])).unwrap();
        assert!(store.get_analysis("old").is_none());
        assert!(store.get_analysis("recent").is_some());
        assert!(store.get_analysis("kept").is_some());
        // And so is the next start.
        store.tidy(now + kept + day).unwrap();
        assert!(store.get_analysis("recent").is_none());
        assert!(store.get_analysis("kept").is_none());
    }

    #[test]
    fn what_an_older_build_did_to_a_row_is_set_straight_at_the_next_start() {
        let store = temp_store();
        let (a, b) = ("https://a.home", "https://b.home");
        store.record_server(a, "s1").unwrap();
        store.record_server(b, "s2").unwrap();
        for id in ["claimed", "released", "reanalysed", "back", "untouched"] {
            store
                .put_analysis_from(Some(&server(a, "s1")), id, "{}")
                .unwrap();
        }
        store
            .release_analyses(&server(a, "s1"), &list(&["back"]))
            .unwrap();
        // What 0.6.1 does, by address alone: a claim for b, a release, a
        // new analysis under b, a claim back of a released row.
        store
            .with_conn(|conn| {
                conn.execute_batch(
                    "UPDATE track_analysis SET server_url = 'https://b.home'
                         WHERE item_id IN ('claimed', 'reanalysed', 'back');
                     UPDATE track_analysis SET server_url = '' WHERE item_id = 'released';",
                )
            })
            .unwrap();
        // Until the next start, a's index still has the rows b took.
        assert_eq!(
            ids_of(&store, &server(a, "s1")),
            ["claimed", "reanalysed", "untouched"]
        );
        store.tidy(super::now_ms_i64()).unwrap();
        assert_eq!(row(&store, "claimed"), (Some(b.into()), None));
        assert_eq!(row(&store, "released"), (Some(RELEASED.into()), None));
        assert!(released_at(&store, "released").is_some());
        assert_eq!(released_at(&store, "back"), None);
        assert_eq!(ids_of(&store, &server(a, "s1")), ["untouched"]);
        assert_eq!(
            ids_of(&store, &server(b, "s2")),
            ["back", "claimed", "reanalysed"]
        );
    }

    #[test]
    fn the_server_seen_last_at_an_address_wins_even_past_a_clock_that_went_back() {
        let store = temp_store();
        let at = "https://music.home";
        assert_eq!(store.server_id_at(at), None);
        store.record_server(at, "s1").unwrap();
        store
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE server_address SET last_seen = last_seen + 86400000 WHERE server_id = 's1'",
                    [],
                )
            })
            .unwrap();
        store.record_server(at, "s2").unwrap();
        assert_eq!(store.server_id_at(at).as_deref(), Some("s2"));
        store.record_server(at, "s1").unwrap();
        assert_eq!(store.server_id_at(at).as_deref(), Some("s1"));
    }

    #[test]
    fn an_extras_entry_that_is_not_json_is_skipped_not_reset() {
        let store = temp_store();
        let mine = r#"{"recordHistory":false,"autoDjEnabled":true}"#;
        store.set(keys::EXTRAS, mine).unwrap();
        // A hand-edited export with a trailing comma inside the blob.
        let export = serde_json::json!({
            "extras": r#"{"recordHistory":false,}"#,
            "volume": "0.5",
        })
        .to_string();
        assert_eq!(store.import_settings(&export).unwrap(), ["volume"]);
        assert_eq!(store.get(keys::EXTRAS).as_deref(), Some(mine));
        // Nor does an unreadable stored blob go out as an empty one.
        store.set(keys::EXTRAS, "not json").unwrap();
        let exported: serde_json::Value =
            serde_json::from_str(&store.export_settings().unwrap()).unwrap();
        assert!(exported.get("extras").is_none(), "{exported}");
    }

    fn temp_store() -> TempStore {
        let dir =
            std::env::temp_dir().join(format!("sonarune-store-test-{}", uuid::Uuid::new_v4()));
        TempStore {
            store: Some(Store::open(&dir).expect("open test store")),
            dir,
        }
    }

    const SERVER: &str = "https://music.home";
    const OLD: &str = "AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA";
    const NEW: &str = "BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB:BB";

    #[test]
    fn auto_trust_is_kept_only_for_a_server_with_a_pin() {
        let t = temp_store();
        let auto_trust = |t: &TempStore| t.get(keys::ACCEPT_INVALID_CERTS);
        // Ticked, but nothing got pinned: the server has a publicly valid
        // certificate, so the preference must not survive the login.
        t.settle_auto_trust(SERVER, true).unwrap();
        assert_eq!(auto_trust(&t).as_deref(), Some("false"));
        // A self-signed server trusted on first use is pinned: it stays.
        t.pin_fingerprint(SERVER, OLD).unwrap();
        t.settle_auto_trust(SERVER, true).unwrap();
        assert_eq!(auto_trust(&t).as_deref(), Some("true"));
        // Unticked is always off.
        t.settle_auto_trust(SERVER, false).unwrap();
        assert_eq!(auto_trust(&t).as_deref(), Some("false"));
        // Another server's pin does not count.
        t.settle_auto_trust("https://other.home", true).unwrap();
        assert_eq!(auto_trust(&t).as_deref(), Some("false"));
    }

    #[test]
    fn pinning_a_changed_certificate_replaces_the_old_pin() {
        let t = temp_store();
        t.pin_fingerprint(SERVER, OLD).unwrap();
        t.pin_fingerprint(SERVER, NEW).unwrap();
        assert_eq!(t.trusted_fingerprints(SERVER), vec![NEW.to_string()]);
    }

    #[test]
    fn pins_stay_per_server_and_carry_a_date() {
        let t = temp_store();
        t.pin_fingerprint(SERVER, OLD).unwrap();
        t.pin_fingerprint("https://other.home", NEW).unwrap();
        assert_eq!(t.trusted_fingerprints(SERVER), vec![OLD.to_string()]);
        let certs = t.trusted_certs();
        assert_eq!(certs.len(), 2);
        assert!(certs.iter().all(|c| c.pinned_at.is_some()));
    }

    #[test]
    fn repinning_the_same_certificate_keeps_its_date() {
        let t = temp_store();
        t.pin_fingerprint(SERVER, OLD).unwrap();
        let first = t.trusted_certs()[0].pinned_at;
        // Same certificate in another spelling.
        t.pin_fingerprint(SERVER, &OLD.replace(':', "").to_lowercase())
            .unwrap();
        assert_eq!(t.trusted_certs()[0].pinned_at, first);
    }

    #[test]
    fn an_invalid_fingerprint_is_never_pinned() {
        let t = temp_store();
        assert!(t.pin_fingerprint(SERVER, "AB:CD").is_err());
        assert!(t.trusted_fingerprints(SERVER).is_empty());
    }

    #[test]
    fn forgetting_removes_the_pin_and_the_empty_server() {
        let t = temp_store();
        t.pin_fingerprint(SERVER, OLD).unwrap();
        assert!(!t.forget_fingerprint(SERVER, NEW).unwrap());
        assert!(t.forget_fingerprint(SERVER, &OLD.to_lowercase()).unwrap());
        assert!(t.trusted_fingerprints(SERVER).is_empty());
        assert!(t.trusted_certs().is_empty());
        assert!(!t.forget_fingerprint(SERVER, OLD).unwrap());
    }

    #[test]
    fn legacy_bare_string_pins_are_still_read_and_forgettable() {
        let t = temp_store();
        t.set(
            keys::TRUSTED_CERTS,
            &serde_json::json!({ SERVER: [OLD] }).to_string(),
        )
        .unwrap();
        assert_eq!(t.trusted_fingerprints(SERVER), vec![OLD.to_string()]);
        let certs = t.trusted_certs();
        assert_eq!(certs[0].fingerprint, OLD);
        assert_eq!(certs[0].pinned_at, None);
        assert!(t.forget_fingerprint(SERVER, OLD).unwrap());
        assert!(t.trusted_fingerprints(SERVER).is_empty());
    }
}
