use crate::error::AppResult;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

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
/// account that scanned. It stays — another account may still see the track,
/// and a scan that lists it again claims it back without a download — but no
/// server's index offers it in the meantime.
const RELEASED: &str = "";

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
        // Which server an analysis belongs to: the sonic index must only
        // offer tracks of the library signed in now. Rows from before the
        // column existed stay NULL and count for every server until a scan
        // whose library lists them claims them (`claim_analyses`).
        let has_server: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('track_analysis') WHERE name = 'server_url'")?
            .exists([])?;
        if !has_server {
            conn.execute_batch("ALTER TABLE track_analysis ADD COLUMN server_url TEXT")?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
            trust: Mutex::new(()),
            analysis_generation: std::sync::atomic::AtomicU64::new(0),
        })
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

    /// [`get_analysis`](Self::get_analysis), with the server the analysis
    /// is tagged with (`None` for rows from before analyses recorded it).
    pub fn analysis_row(&self, item_id: &str) -> Option<(String, Option<String>)> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT json, server_url FROM track_analysis WHERE item_id = ?1",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
    }

    /// Store an analysis made on the server signed in now (the player's).
    pub fn put_analysis(&self, item_id: &str, json: &str) -> AppResult<()> {
        let server = self.get(keys::SERVER_URL);
        self.put_analysis_from(server.as_deref(), item_id, json)
    }

    /// Store an analysis made on `server`: the base URL the audio came from.
    /// The library scan says so itself, because a download can still be
    /// finishing after a sign-in to another server.
    pub fn put_analysis_from(
        &self,
        server: Option<&str>,
        item_id: &str,
        json: &str,
    ) -> AppResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO track_analysis (item_id, json, server_url) VALUES (?1, ?2, ?3)
             ON CONFLICT(item_id) DO UPDATE SET json = excluded.json,
                                                server_url = excluded.server_url",
            rusqlite::params![item_id, json, server],
        )?;
        self.analysis_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Tag these analyses as `server`'s: its library lists the tracks, so
    /// they belong to it, whatever address they were analysed under — the
    /// same server reached under a new URL keeps its index. Returns how many
    /// moved.
    pub fn claim_analyses(&self, server: &str, item_ids: &[String]) -> AppResult<usize> {
        self.retag_analyses(
            "UPDATE track_analysis SET server_url = ?1
             WHERE server_url IS NOT ?2 AND item_id = ?3",
            server,
            server,
            item_ids,
        )
    }

    /// Take these analyses of `server` out of its index ([`RELEASED`]): a
    /// complete scan of its library did not list them. Returns how many.
    pub fn release_analyses(&self, server: &str, item_ids: &[String]) -> AppResult<usize> {
        self.retag_analyses(
            "UPDATE track_analysis SET server_url = ?1
             WHERE server_url = ?2 AND item_id = ?3",
            RELEASED,
            server,
            item_ids,
        )
    }

    /// Tag every analysis made under one of `addresses` as `server`'s: they
    /// are other addresses of the same server (`history::other_addresses`).
    /// Released rows stay released. Returns how many moved.
    pub fn adopt_analyses(&self, server: &str, addresses: &[String]) -> AppResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut moved = 0;
        {
            let mut statement =
                tx.prepare("UPDATE track_analysis SET server_url = ?1 WHERE server_url = ?2")?;
            for address in addresses {
                if address != RELEASED && address != server {
                    moved += statement.execute(rusqlite::params![server, address])?;
                }
            }
        }
        tx.commit()?;
        if moved > 0 {
            self.analysis_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(moved)
    }

    /// Run `sql` with (`to`, `from`, item id) for every id.
    fn retag_analyses(
        &self,
        sql: &str,
        to: &str,
        from: &str,
        item_ids: &[String],
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
                changed += statement.execute(rusqlite::params![to, from, item_id])?;
            }
        }
        tx.commit()?;
        if changed > 0 {
            self.analysis_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(changed)
    }

    /// Item ids whose analysis is tagged with exactly `server`.
    pub fn analysis_ids_of(&self, server: &str) -> AppResult<Vec<String>> {
        self.with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT item_id FROM track_analysis WHERE server_url = ?1")?;
            let ids = statement
                .query_map([server], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ids)
        })
    }

    /// Every stored analysis of `server` (or from before analyses recorded
    /// their server), as (item id, JSON). Released ones are no server's.
    pub fn analyses_for(&self, server: &str) -> AppResult<Vec<(String, String)>> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT item_id, json FROM track_analysis
                 WHERE server_url = ?1 OR server_url IS NULL",
            )?;
            let rows = statement
                .query_map([server], |row| Ok((row.get(0)?, row.get(1)?)))?
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
mod tests {
    use super::{keys, Store};
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

    #[test]
    fn analyses_are_listed_for_the_server_they_were_made_on() {
        let store = temp_store();
        store.set(keys::SERVER_URL, "https://a.home").unwrap();
        store.put_analysis("a1", "{}").unwrap();
        store.set(keys::SERVER_URL, "https://b.home").unwrap();
        store.put_analysis("b1", "{}").unwrap();
        let ids = |server: &str| {
            let mut ids: Vec<String> = store
                .analyses_for(server)
                .unwrap()
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(ids("https://a.home"), ["a1"]);
        assert_eq!(ids("https://b.home"), ["b1"]);
        // Re-analysed on b: it moves there.
        store.put_analysis("a1", "{}").unwrap();
        assert!(ids("https://a.home").is_empty());
        // The scan names the server the audio came from, whatever is signed
        // in by the time the download is done.
        store
            .put_analysis_from(Some("https://a.home"), "a2", "{}")
            .unwrap();
        assert_eq!(ids("https://a.home"), ["a2"]);
    }

    #[test]
    fn a_listed_track_is_claimed_and_an_unlisted_one_released() {
        let store = temp_store();
        let old = "http://10.0.0.5:8096";
        let new = "https://music.example.org";
        store.put_analysis_from(Some(old), "t1", "{}").unwrap();
        store.put_analysis_from(Some(old), "t2", "{}").unwrap();
        store.put_analysis_from(None, "legacy", "{}").unwrap();
        let ids = |server: &str| {
            let mut ids: Vec<String> = store
                .analyses_for(server)
                .unwrap()
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            ids.sort();
            ids
        };
        let list = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        // The same server under a new address: what its library lists is
        // its own, without a download.
        let generation = store.analysis_generation();
        assert_eq!(
            store.claim_analyses(new, &list(&["t1", "legacy"])).unwrap(),
            2
        );
        assert!(
            store.analysis_generation() > generation,
            "the index rebuilds"
        );
        assert_eq!(ids(new), ["legacy", "t1"]);
        assert_eq!(ids(old), ["t2"]);
        assert_eq!(store.claim_analyses(new, &list(&["t1"])).unwrap(), 0);
        // A complete scan did not list t1 any more: it leaves every index,
        // but its analysis stays.
        assert_eq!(store.release_analyses(new, &list(&["t1"])).unwrap(), 1);
        assert_eq!(ids(new), ["legacy"]);
        assert!(!ids(old).contains(&"t1".to_string()));
        assert!(store.get_analysis("t1").is_some());
        // Listed again: claimed back.
        assert_eq!(store.claim_analyses(new, &list(&["t1"])).unwrap(), 1);
        assert_eq!(ids(new), ["legacy", "t1"]);
        // A server only releases its own.
        assert_eq!(store.release_analyses(new, &list(&["t2"])).unwrap(), 0);
        assert_eq!(store.analysis_ids_of(old).unwrap(), ["t2"]);
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
