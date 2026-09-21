//! Detects server-side music-library changes (new music added, tracks
//! removed) and notifies the frontend so it can refresh.
//!
//! A light poll rather than the Jellyfin WebSocket: it reuses the existing
//! reqwest session (self-signed certs, auth, relogin all handled) and adds no
//! dependency. A `check_now` poke — fired when the window regains focus —
//! makes it feel immediate after adding music on the server.

use crate::api::LibrarySignature;
use crate::player::SharedSession;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

/// Background poll cadence: short enough to feel automatic, cheap enough to
/// ignore (one `limit=1` request per tick).
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Whose library a signature describes: server URL and user id. Users on one
/// server can see different libraries.
type LibraryIdentity = (String, String);

/// True only when the SAME server and user's signature changed — never on the
/// first poll of a (re)connected session, a switched server, or another
/// account on the same server.
fn library_changed(
    last: Option<&(LibraryIdentity, LibrarySignature)>,
    identity: &LibraryIdentity,
    sig: &LibrarySignature,
) -> bool {
    last.is_some_and(|(prev_identity, prev)| prev_identity == identity && prev != sig)
}

#[derive(Clone)]
pub struct LibraryWatchHandle {
    notify: Arc<Notify>,
}

impl LibraryWatchHandle {
    /// Trigger an immediate library check (e.g. on window focus).
    pub fn check_now(&self) {
        self.notify.notify_one();
    }
}

pub fn spawn(app: AppHandle, session: SharedSession) -> LibraryWatchHandle {
    let notify = Arc::new(Notify::new());
    let handle = LibraryWatchHandle {
        notify: notify.clone(),
    };
    tauri::async_runtime::spawn(async move {
        // Baseline is keyed by server URL and user id so a switch to a
        // different server or account (even without the loop observing the
        // disconnect) re-baselines instead of comparing signatures across
        // libraries.
        let mut last: Option<(LibraryIdentity, LibrarySignature)> = None;
        loop {
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
            let client = session.read().await.clone();
            let Some(client) = client else {
                last = None;
                continue;
            };
            let identity = (
                client.base_url().to_string(),
                client.user_id().unwrap_or_default().to_string(),
            );
            // Let the UI show a "checking" state; a fast poll finishes before
            // the frontend's spinner debounce elapses, so it stays invisible
            // for the routine 30 s ticks and only shows on a slow/manual check.
            let _ = app.emit("library:sync", "checking");
            match client.library_signature().await {
                Ok(sig) => {
                    if library_changed(last.as_ref(), &identity, &sig) {
                        tracing::debug!("library changed on server");
                        let _ = app.emit("library:changed", ());
                    }
                    last = Some((identity, sig));
                    let _ = app.emit("library:sync", "idle");
                }
                Err(e) => {
                    tracing::debug!("library poll failed: {e}");
                    let _ = app.emit("library:sync", "error");
                }
            }
        }
    });
    handle
}

#[cfg(test)]
mod tests {
    use super::{library_changed, LibraryIdentity};
    use crate::api::LibrarySignature;

    fn identity(server: &str, user: &str) -> LibraryIdentity {
        (server.to_string(), user.to_string())
    }

    fn sig(count: i64) -> LibrarySignature {
        LibrarySignature {
            newest: "2026-01-01T00:00:00Z".into(),
            count,
        }
    }

    #[test]
    fn only_the_same_server_and_user_can_report_a_change() {
        let alice = identity("https://jf.example", "alice");
        let bob = identity("https://jf.example", "bob");
        let other_server = identity("https://other.example", "alice");
        let baseline = (alice.clone(), sig(10));

        // First poll after connecting: nothing to compare against.
        assert!(!library_changed(None, &alice, &sig(10)));
        assert!(!library_changed(Some(&baseline), &alice, &sig(10)));
        assert!(library_changed(Some(&baseline), &alice, &sig(11)));
        // Account switch on the same server: a different library, not a change.
        assert!(!library_changed(Some(&baseline), &bob, &sig(3)));
        assert!(!library_changed(Some(&baseline), &other_server, &sig(3)));
    }
}
