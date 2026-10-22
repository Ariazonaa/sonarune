//! Opening the database at startup, and what happens when that fails.
//!
//! An error out of `setup` is fatal to Tauri — it panics — and the release
//! build has no console, so the app would simply vanish, on every start. A
//! damaged database is set aside next to where it was and replaced by a fresh
//! one; the setup screen then says where the old one went
//! (`get_database_recovery`). Anything else — a full disk, a file that is
//! read-only or locked — can go away again and must not cost the user their
//! data: the app names the file and the error, and quits.

use crate::error::AppError;
use crate::store::{Store, DB_FILE};
use rusqlite::ErrorCode;
use std::path::{Path, PathBuf};

/// Where a damaged database was moved at this start, if it was.
pub struct DatabaseRecovery(pub Option<String>);

/// The store in `dir`, or a fresh one in place of a damaged file. Does not
/// return when neither can be opened.
pub fn open(dir: &Path) -> (Store, DatabaseRecovery) {
    match open_or_recover(dir) {
        Ok((store, moved)) => (
            store,
            DatabaseRecovery(moved.map(|path| path.display().to_string())),
        ),
        Err(error) => fatal(&dir.join(DB_FILE), &error),
    }
}

fn open_or_recover(dir: &Path) -> Result<(Store, Option<PathBuf>), AppError> {
    let error = match Store::open(dir) {
        Ok(store) => return Ok((store, None)),
        Err(error) if is_damaged(&error) => error,
        Err(error) => return Err(error),
    };
    tracing::warn!("the database is damaged ({error}), setting it aside");
    let moved = set_aside(dir, &now_secs().to_string())
        .map_err(|e| AppError::Other(format!("{error}; it could not be set aside: {e}")))?;
    tracing::warn!("started with a fresh database");
    Ok((Store::open(dir)?, Some(moved)))
}

/// The file is not a database, or one SQLite can no longer read. Only then is
/// starting over the way out.
fn is_damaged(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Db(rusqlite::Error::SqliteFailure(failure, _))
            if matches!(failure.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

/// The files SQLite keeps next to a database. None may stay behind when the
/// database goes: a log or a hot journal would be replayed into the fresh one.
const COMPANIONS: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Rename the database to `sonarune.db.broken-<suffix>`, its companions along
/// with it, so the set is still readable together. A companion that cannot be
/// renamed is copied over and then removed. When not even that works, what was
/// moved goes back and nothing is set aside: a log may hold committed data, and
/// not starting beats losing it (`open` then says why and quits).
fn set_aside(dir: &Path, suffix: &str) -> std::io::Result<PathBuf> {
    let original = dir.join(DB_FILE);
    let moved = dir.join(format!("{DB_FILE}.broken-{suffix}"));
    let beside = |base: &Path, companion: &str| {
        let mut path = base.as_os_str().to_os_string();
        path.push(companion);
        PathBuf::from(path)
    };
    std::fs::rename(&original, &moved)?;
    let mut done: Vec<&str> = Vec::new();
    for companion in COMPANIONS {
        let (from, to) = (beside(&original, companion), beside(&moved, companion));
        if !from.exists() {
            continue;
        }
        let result = std::fs::rename(&from, &to).or_else(|_| {
            std::fs::copy(&from, &to)?;
            std::fs::remove_file(&from)
        });
        if let Err(e) = result {
            for companion in done {
                let _ = std::fs::rename(beside(&moved, companion), beside(&original, companion));
            }
            let _ = std::fs::rename(&moved, &original);
            return Err(e);
        }
        done.push(companion);
    }
    Ok(moved)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Say what went wrong, in both of the app's languages — there is no window
/// to pick one in yet — and quit.
fn fatal(path: &Path, error: &AppError) -> ! {
    tracing::error!("cannot open the database at {}: {error}", path.display());
    let text = ["en", "de"]
        .map(|locale| message(locale, path, error))
        .join("\n\n");
    show_error(&text);
    std::process::exit(1);
}

/// `startup_database_failed` from `messages/<locale>.json`, where every
/// user-facing string lives.
fn message(locale: &str, path: &Path, error: &AppError) -> String {
    let messages = match locale {
        "de" => include_str!("../../messages/de.json"),
        _ => include_str!("../../messages/en.json"),
    };
    let template = serde_json::from_str::<serde_json::Value>(messages)
        .ok()
        .and_then(|all| all["startup_database_failed"].as_str().map(str::to_string))
        .unwrap_or_else(|| "{path}: {error}".to_string());
    template
        .replace("{path}", &path.display().to_string())
        .replace("{error}", &error.to_string())
}

#[cfg(windows)]
fn show_error(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(text), wide("Sonarune"));
    // SAFETY: both strings are NUL-terminated UTF-16 and outlive the call; a
    // null owner window is allowed.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn show_error(text: &str) {
    eprintln!("{text}");
}

/// Where a damaged database was set aside at this start; `None` normally.
#[tauri::command]
pub fn get_database_recovery(recovery: tauri::State<'_, DatabaseRecovery>) -> Option<String> {
    recovery.0.clone()
}

#[cfg(test)]
mod tests {
    use super::{is_damaged, message, open_or_recover, set_aside};
    use crate::error::AppError;
    use crate::store::{keys, DB_FILE};
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        let path =
            std::env::temp_dir().join(format!("sonarune-recovery-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create test directory");
        TempDir(path)
    }

    #[test]
    fn a_damaged_database_is_set_aside_and_replaced() {
        let dir = temp_dir();
        let garbage = b"this was a database once".repeat(400);
        std::fs::write(dir.0.join(DB_FILE), &garbage).unwrap();

        let (store, moved) = open_or_recover(&dir.0).expect("recovered");
        let moved = moved.expect("the damaged file was moved");
        // The fresh store works, and the old file is kept for whoever wants
        // to look at it.
        store.set(keys::VOLUME, "0.5").unwrap();
        assert_eq!(store.get(keys::VOLUME).as_deref(), Some("0.5"));
        assert_eq!(std::fs::read(&moved).unwrap(), garbage);
        drop(store);
    }

    #[test]
    fn the_log_files_go_along_with_the_database() {
        let dir = temp_dir();
        std::fs::write(dir.0.join(DB_FILE), b"pages").unwrap();
        std::fs::write(dir.0.join(format!("{DB_FILE}-wal")), b"log").unwrap();
        std::fs::write(dir.0.join(format!("{DB_FILE}-shm")), b"index").unwrap();
        std::fs::write(dir.0.join(format!("{DB_FILE}-journal")), b"journal").unwrap();
        let moved = set_aside(&dir.0, "7").unwrap();
        assert_eq!(moved, dir.0.join(format!("{DB_FILE}.broken-7")));
        let companion = |suffix: &str| {
            let mut path = moved.clone().into_os_string();
            path.push(suffix);
            std::fs::read(path).unwrap()
        };
        assert_eq!(std::fs::read(&moved).unwrap(), b"pages");
        assert_eq!(companion("-wal"), b"log");
        assert_eq!(companion("-shm"), b"index");
        assert_eq!(companion("-journal"), b"journal");
        // Nothing is left behind to be replayed into a fresh database.
        for name in [
            DB_FILE.to_string(),
            format!("{DB_FILE}-wal"),
            format!("{DB_FILE}-shm"),
            format!("{DB_FILE}-journal"),
        ] {
            assert!(!dir.0.join(name).exists());
        }
    }

    #[test]
    fn a_log_that_cannot_be_kept_keeps_everything_where_it_was() {
        let dir = temp_dir();
        std::fs::write(dir.0.join(DB_FILE), b"pages").unwrap();
        std::fs::write(dir.0.join(format!("{DB_FILE}-wal")), b"log").unwrap();
        std::fs::write(dir.0.join(format!("{DB_FILE}-shm")), b"index").unwrap();
        // Where the shared-memory file would go, a folder is in the way:
        // neither a rename nor a copy can put it there.
        std::fs::create_dir(dir.0.join(format!("{DB_FILE}.broken-7-shm"))).unwrap();
        std::fs::write(dir.0.join(format!("{DB_FILE}.broken-7-shm/x")), b"").unwrap();
        assert!(set_aside(&dir.0, "7").is_err());
        // The log was not deleted, and the set is back as it was found.
        assert_eq!(std::fs::read(dir.0.join(DB_FILE)).unwrap(), b"pages");
        assert_eq!(
            std::fs::read(dir.0.join(format!("{DB_FILE}-wal"))).unwrap(),
            b"log"
        );
        assert_eq!(
            std::fs::read(dir.0.join(format!("{DB_FILE}-shm"))).unwrap(),
            b"index"
        );
        assert!(!dir.0.join(format!("{DB_FILE}.broken-7")).exists());
    }

    #[test]
    fn a_healthy_database_is_opened_as_it_is() {
        let dir = temp_dir();
        {
            let (store, moved) = open_or_recover(&dir.0).unwrap();
            assert!(moved.is_none());
            store.set(keys::VOLUME, "0.25").unwrap();
        }
        let (store, moved) = open_or_recover(&dir.0).unwrap();
        assert!(moved.is_none());
        assert_eq!(store.get(keys::VOLUME).as_deref(), Some("0.25"));
    }

    #[test]
    fn only_damage_counts_as_damage() {
        let damaged = AppError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            None,
        ));
        assert!(is_damaged(&damaged));
        for code in [
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_CANTOPEN,
        ] {
            let error = AppError::Db(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ));
            assert!(!is_damaged(&error), "{error}");
        }
        assert!(!is_damaged(&AppError::Other(
            "cannot create data dir".into()
        )));
    }

    #[test]
    fn nothing_is_moved_when_the_database_cannot_be_renamed() {
        let dir = temp_dir();
        assert!(set_aside(&dir.0, "1").is_err());
        assert!(!dir.0.join(format!("{DB_FILE}.broken-1")).exists());
    }

    #[test]
    fn the_startup_message_exists_in_both_languages() {
        let path = Path::new(r"C:\Users\someone\AppData\Roaming\io.sonarune.app\sonarune.db");
        let error = AppError::Other("database or disk is full".into());
        for locale in ["en", "de"] {
            let text = message(locale, path, &error);
            assert!(text.contains("sonarune.db"), "{locale}: {text}");
            assert!(
                text.contains("database or disk is full"),
                "{locale}: {text}"
            );
            assert!(!text.contains('{'), "{locale}: {text}");
        }
        assert_ne!(message("en", path, &error), message("de", path, &error));
    }
}
