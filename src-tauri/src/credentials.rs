//! The credential store, and what happens without one.
//!
//! Tokens and passwords live in the OS credential store: the Credential
//! Manager on Windows, the Secret Service on Linux (GNOME Keyring, KWallet),
//! the keychain on macOS. A Linux desktop may have none — a bare window
//! manager without a keyring daemon — and then every keyring call fails with
//! `NoDefaultStore`; a locked store whose unlock the user dismissed answers
//! `NoStorageAccess`. The sign-in failed with that, and the app could not be
//! used at all (seen on the test VM, 07.11.2026).
//!
//! Such secrets are now held in memory for the rest of the run, and nowhere
//! else: tokens stay out of files (the store and the settings are plain
//! SQLite), so the next start asks for the sign-in again. [`persistent`] tells
//! the UI, which says so.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Secrets the credential store could not take, by (service, user).
static MEMORY: Mutex<Option<HashMap<(String, String), String>>> = Mutex::new(None);
/// Set once the credential store turned a secret away.
static MISSING: AtomicBool = AtomicBool::new(false);

/// Whether `err` means "there is no store to keep it in", as opposed to a
/// store that answered (no such entry, a value it cannot read).
fn no_store(err: &keyring::Error) -> bool {
    matches!(
        err,
        keyring::Error::NoDefaultStore | keyring::Error::NoStorageAccess(_)
    )
}

fn note_missing(err: &keyring::Error) {
    if !MISSING.swap(true, Ordering::Relaxed) {
        tracing::warn!("no credential store, keeping the sign-in for this run only: {err}");
    }
}

fn remember(key: &(String, String), secret: &str) {
    MEMORY
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key.clone(), secret.to_string());
}

fn recall(key: &(String, String)) -> Option<String> {
    MEMORY.lock().unwrap().as_ref()?.get(key).cloned()
}

fn forget(key: &(String, String)) -> bool {
    MEMORY
        .lock()
        .unwrap()
        .as_mut()
        .is_some_and(|memory| memory.remove(key).is_some())
}

/// Whether the secrets of this run are in the credential store, and will be
/// there at the next start.
pub fn persistent() -> bool {
    !MISSING.load(Ordering::Relaxed)
}

/// One secret: a [`keyring::Entry`] where there is a store, this run's memory
/// where there is none. The same calls as the entry's.
pub struct Secret {
    key: (String, String),
    entry: Option<keyring::Entry>,
}

impl Secret {
    pub fn new(service: &str, user: &str) -> keyring::Result<Self> {
        let entry = match keyring::Entry::new(service, user) {
            Ok(entry) => Some(entry),
            Err(err) if no_store(&err) => {
                note_missing(&err);
                None
            }
            Err(err) => return Err(err),
        };
        Ok(Self {
            key: (service.to_string(), user.to_string()),
            entry,
        })
    }

    pub fn set_password(&self, secret: &str) -> keyring::Result<()> {
        if let Some(entry) = &self.entry {
            match entry.set_password(secret) {
                Err(err) if no_store(&err) => note_missing(&err),
                Ok(()) => {
                    // A copy from before the store came up is out of date.
                    forget(&self.key);
                    return Ok(());
                }
                Err(err) => return Err(err),
            }
        }
        remember(&self.key, secret);
        Ok(())
    }

    pub fn get_password(&self) -> keyring::Result<String> {
        // What this run had to keep itself is the newest value: a store that
        // turned the write away may still answer reads with an older one.
        if let Some(secret) = recall(&self.key) {
            return Ok(secret);
        }
        if let Some(entry) = &self.entry {
            match entry.get_password() {
                Err(err) if no_store(&err) => note_missing(&err),
                Err(keyring::Error::NoEntry) => {}
                other => return other,
            }
        }
        recall(&self.key).ok_or(keyring::Error::NoEntry)
    }

    pub fn delete_credential(&self) -> keyring::Result<()> {
        let in_memory = forget(&self.key);
        if let Some(entry) = &self.entry {
            match entry.delete_credential() {
                Err(err) if no_store(&err) => {}
                Err(keyring::Error::NoEntry) if in_memory => {}
                other => return other,
            }
            return Ok(());
        }
        if in_memory {
            Ok(())
        } else {
            Err(keyring::Error::NoEntry)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A secret with no store behind it, as `Secret::new` makes one when the
    /// platform has none.
    fn without_store(user: &str) -> Secret {
        Secret {
            key: ("sonarune-credentials-test".into(), user.into()),
            entry: None,
        }
    }

    #[test]
    fn without_a_store_the_secret_lasts_the_run() {
        let token = without_store("http://s::u");
        assert!(matches!(token.get_password(), Err(keyring::Error::NoEntry)));
        token.set_password("t1").unwrap();
        assert_eq!(token.get_password().unwrap(), "t1");
        // Another handle on the same name sees it, as with a store.
        assert_eq!(without_store("http://s::u").get_password().unwrap(), "t1");
        token.set_password("t2").unwrap();
        assert_eq!(token.get_password().unwrap(), "t2");
        // Another name does not.
        assert!(without_store("http://s::v").get_password().is_err());
        token.delete_credential().unwrap();
        assert!(matches!(token.get_password(), Err(keyring::Error::NoEntry)));
        assert!(matches!(
            token.delete_credential(),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn only_a_missing_or_closed_store_counts_as_no_store() {
        assert!(no_store(&keyring::Error::NoDefaultStore));
        assert!(no_store(&keyring::Error::NoStorageAccess(
            "locked".to_string().into()
        )));
        assert!(!no_store(&keyring::Error::NoEntry));
        assert!(!no_store(&keyring::Error::PlatformFailure(
            "broken".to_string().into()
        )));
    }
}
