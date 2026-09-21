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
}

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
}

impl Store {
    pub fn open(dir: &Path) -> AppResult<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::error::AppError::Other(format!("cannot create data dir: {e}")))?;
        let conn = Connection::open(dir.join("jellysic.db"))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS kv (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            trust: Mutex::new(()),
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
        ] {
            if let Some(value) = self.get(key) {
                let value = if key == keys::EXTRAS {
                    strip_extras_token(&value)
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
        ] {
            if let Some(serde_json::Value::String(value)) = map.get(key) {
                let value = match key {
                    // A hand-edited export must not poison the output gain.
                    keys::VOLUME => match value.parse::<f32>() {
                        Ok(v) if v.is_finite() => v.clamp(0.0, 1.0).to_string(),
                        _ => continue,
                    },
                    // Imports never carry a credential into the store.
                    keys::EXTRAS => {
                        let stripped = strip_extras_token(value);
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
/// the non-secret fields.
fn strip_extras_token(extras_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(extras_json) {
        Ok(mut value) => {
            if let Some(obj) = value.as_object_mut() {
                obj.remove("listenbrainzToken");
            }
            value.to_string()
        }
        // Unparseable blob: drop it rather than risk leaking a credential.
        Err(_) => "{}".to_string(),
    }
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

    fn temp_store() -> TempStore {
        let dir =
            std::env::temp_dir().join(format!("jellysic-store-test-{}", uuid::Uuid::new_v4()));
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
