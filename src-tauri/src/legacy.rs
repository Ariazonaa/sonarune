//! Carrying installs over from the old name.
//!
//! Up to 0.5.1 the app was called Jellysic: its identifier was
//! `io.jellysic.app`, its database `jellysic.db` and its keyring service
//! `jellysic`. Each of those decides where a piece of state lives, so renaming
//! them without moving the state would sign every user out and reset every
//! setting. Each step here runs only while the new location is still empty, so
//! it is a one-off: once anything has been written under the new name, the old
//! one is never looked at again.
//!
//! The WebView's `localStorage` moves with the local data directory; its keys
//! are renamed by the pre-paint script in `app.html`. The old installation
//! itself (program folder, shortcut, uninstall entry) is removed by the
//! installer hook in `installer/hooks.nsh`.

use std::path::Path;
use std::time::Duration;

/// The identifier the old one was renamed to. A build under any other one (the
/// demo build behind the README screenshots, a test build) has no claim on the
/// old data and must leave it where it is.
pub const IDENTIFIER: &str = "io.sonarune.app";
pub const OLD_IDENTIFIER: &str = "io.jellysic.app";
pub const OLD_DB_FILE: &str = "jellysic.db";
pub const OLD_KEYRING_SERVICE: &str = "jellysic";

/// Move the old identifier's directories to the new identifier's, before
/// Tauri, the window-state plugin or WebView2 open anything inside them.
///
/// Tauri resolves its directories through `SHGetKnownFolderPath`, which is
/// what the `dirs` crate calls too, so these are the same base folders. On
/// Windows that is Roaming (database, window state) and Local (WebView2
/// profile with `localStorage`, cover cache); elsewhere the set is wider, and
/// duplicates simply find nothing left to move.
pub fn migrate_data_dirs(identifier: &str) {
    if identifier != IDENTIFIER {
        return;
    }
    let bases = [
        dirs::data_dir(),
        dirs::data_local_dir(),
        dirs::config_dir(),
        dirs::cache_dir(),
    ];
    for base in bases.into_iter().flatten() {
        move_dir(&base.join(OLD_IDENTIFIER), &base.join(identifier));
    }
}

fn move_dir(old: &Path, new: &Path) {
    if !old.is_dir() || new.exists() {
        return;
    }
    // Right after an update the old process, or a WebView2 helper it left
    // behind, may still hold a file open for a moment. Wait it out rather than
    // start fresh and leave everything in the old folder.
    let mut last_err = None;
    for _ in 0..20 {
        match std::fs::rename(old, new) {
            Ok(()) => {
                tracing::info!("moved {} to {}", old.display(), new.display());
                return;
            }
            Err(e) => last_err = Some(e),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if let Some(e) = last_err {
        tracing::warn!(
            "could not move {} to {}: {e}; starting with empty state",
            old.display(),
            new.display()
        );
    }
}

/// Rename the old database file (with its WAL and shared-memory files) to
/// `db`, inside the already migrated data directory.
pub fn migrate_db_file(dir: &Path, db: &str) {
    let new = dir.join(db);
    let old = dir.join(OLD_DB_FILE);
    if new.exists() || !old.exists() {
        return;
    }
    // WAL and SHM first: a database without its WAL would lose whatever the
    // last session had not checkpointed yet.
    for suffix in ["-wal", "-shm", ""] {
        let from = dir.join(format!("{OLD_DB_FILE}{suffix}"));
        if from.exists() {
            if let Err(e) = std::fs::rename(&from, dir.join(format!("{db}{suffix}"))) {
                tracing::warn!("could not rename {}: {e}", from.display());
            }
        }
    }
}

/// Move one credential from the old keyring service to `service`, unless the
/// new one already holds a value. The old entry is deleted only once the copy
/// is written.
pub fn migrate_credential(service: &str, user: &str) {
    let Ok(new) = keyring::Entry::new(service, user) else {
        return;
    };
    if new.get_password().is_ok() {
        return;
    }
    let Ok(old) = keyring::Entry::new(OLD_KEYRING_SERVICE, user) else {
        return;
    };
    let Ok(secret) = old.get_password() else {
        return;
    };
    if new.set_password(&secret).is_ok() {
        let _ = old.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::{migrate_db_file, move_dir, OLD_DB_FILE};
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sonarune-legacy-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn identifier_matches_the_app_config() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], super::IDENTIFIER);
    }

    #[test]
    fn old_directory_moves_when_the_new_one_is_missing() {
        let base = temp_dir("move");
        let old = base.join("io.jellysic.app");
        let new = base.join("io.sonarune.app");
        fs::create_dir_all(old.join("EBWebView")).unwrap();
        fs::write(old.join(".window-state.json"), "{}").unwrap();

        move_dir(&old, &new);

        assert!(!old.exists());
        assert_eq!(
            fs::read_to_string(new.join(".window-state.json")).unwrap(),
            "{}"
        );
        assert!(new.join("EBWebView").is_dir());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn existing_new_directory_is_left_alone() {
        let base = temp_dir("keep");
        let old = base.join("io.jellysic.app");
        let new = base.join("io.sonarune.app");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("marker"), "old").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("marker"), "new").unwrap();

        move_dir(&old, &new);

        assert_eq!(fs::read_to_string(old.join("marker")).unwrap(), "old");
        assert_eq!(fs::read_to_string(new.join("marker")).unwrap(), "new");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn database_is_renamed_with_its_wal_and_shm() {
        let dir = temp_dir("db");
        for suffix in ["", "-wal", "-shm"] {
            fs::write(dir.join(format!("{OLD_DB_FILE}{suffix}")), suffix).unwrap();
        }

        migrate_db_file(&dir, "sonarune.db");

        for suffix in ["", "-wal", "-shm"] {
            assert!(!dir.join(format!("{OLD_DB_FILE}{suffix}")).exists());
            let moved = fs::read_to_string(dir.join(format!("sonarune.db{suffix}"))).unwrap();
            assert_eq!(moved, suffix);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn database_is_not_renamed_over_an_existing_one() {
        let dir = temp_dir("db-keep");
        fs::write(dir.join(OLD_DB_FILE), "old").unwrap();
        fs::write(dir.join("sonarune.db"), "new").unwrap();

        migrate_db_file(&dir, "sonarune.db");

        assert_eq!(fs::read_to_string(dir.join(OLD_DB_FILE)).unwrap(), "old");
        assert_eq!(fs::read_to_string(dir.join("sonarune.db")).unwrap(), "new");
        fs::remove_dir_all(&dir).unwrap();
    }
}
