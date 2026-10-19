//! Background analysis of the whole library, for sonic similarity.
//!
//! The player analyses every track it plays anyway (`player/waveform.rs`);
//! this fills in the rest, so a radio or a path can reach tracks that were
//! never played on this machine. Off until the user switches it on. It walks
//! the library a page at a time, skips what is already analysed, and pulls
//! each remaining track as a small file (`JellyfinClient::analysis_audio`) —
//! two at a time, so a home server does not have to transcode more than
//! that at once. Everything is resumable: a track counts as done the moment
//! its analysis is in the store, and a restart picks up from there.

use crate::player::analysis::TrackAnalysis;
use crate::player::SharedSession;
use crate::store::{keys, Store};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

const PAGE: u32 = 500;
const PARALLEL: usize = 2;
/// A finished pass looks for new music this often (and when the library
/// changes, `ScanHandle::poke`).
const IDLE_RECHECK: Duration = Duration::from_secs(30 * 60);
/// After a failed download: wait before the next track, so a server that
/// is down does not get hammered.
const ERROR_BACKOFF: Duration = Duration::from_secs(20);
/// Tracks that failed this many times in one session are skipped until the
/// next start.
const MAX_FAILURES: usize = 2;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatus {
    pub enabled: bool,
    /// A pass is going through the library right now.
    pub running: bool,
    /// Tracks of this library the current sound analysis has run on, out of
    /// `total` (known once a pass has listed the library). Includes the few
    /// too short or too quiet to have a sound to compare.
    pub analysed: u64,
    pub total: Option<u64>,
    /// Name of a track being analysed right now.
    pub current: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct ScanHandle {
    status: Arc<Mutex<ScanStatus>>,
    enabled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl ScanHandle {
    pub fn status(&self) -> ScanStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn set_enabled(&self, store: &Store, enabled: bool) {
        let _ = store.set(keys::SONIC_SCAN, if enabled { "true" } else { "false" });
        self.enabled.store(enabled, Ordering::Relaxed);
        self.status.lock().unwrap().enabled = enabled;
        self.notify.notify_one();
    }

    /// Look again now (library changed, signed in).
    pub fn poke(&self) {
        self.notify.notify_one();
    }
}

fn needs_analysis(store: &Store, item_id: &str, duration_ms: u64) -> bool {
    let Some(json) = store.get_analysis(item_id) else {
        return true;
    };
    match serde_json::from_str::<TrackAnalysis>(&json) {
        Ok(analysis) => {
            analysis.version != crate::player::analysis::VERSION
                || !crate::sonic::sound_attempted(&analysis)
                // The file on the server was replaced.
                || (duration_ms > 0 && (analysis.duration_ms - duration_ms as f64).abs() > 2_000.0)
        }
        Err(_) => true,
    }
}

pub fn spawn(app: AppHandle, session: SharedSession, store: Arc<Store>) -> ScanHandle {
    let enabled = store.get(keys::SONIC_SCAN).as_deref() == Some("true");
    let handle = ScanHandle {
        status: Arc::new(Mutex::new(ScanStatus {
            enabled,
            ..ScanStatus::default()
        })),
        enabled: Arc::new(AtomicBool::new(enabled)),
        notify: Arc::new(Notify::new()),
    };
    let task = handle.clone();
    tauri::async_runtime::spawn(async move {
        let mut failures: std::collections::HashMap<String, usize> = Default::default();
        loop {
            if task.enabled.load(Ordering::Relaxed) {
                pass(&app, &session, &store, &task, &mut failures).await;
            }
            tokio::select! {
                _ = task.notify.notified() => {}
                _ = tokio::time::sleep(IDLE_RECHECK) => {}
            }
        }
    });
    handle
}

fn publish(app: &AppHandle, handle: &ScanHandle, update: impl FnOnce(&mut ScanStatus)) {
    let snapshot = {
        let mut status = handle.status.lock().unwrap();
        update(&mut status);
        status.clone()
    };
    let _ = app.emit("sonic:scan", snapshot);
}

async fn same_session(session: &SharedSession, client: &Arc<crate::api::JellyfinClient>) -> bool {
    session
        .read()
        .await
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, client))
}

/// Fetch, analyse and store one track. `Ok(false)`: analysed, but it has no
/// sound vector (too short or too quiet to compare).
async fn analyse_one(
    client: &crate::api::JellyfinClient,
    store: &Store,
    id: &str,
    duration_ms: u64,
) -> crate::error::AppResult<bool> {
    let bytes = client.analysis_audio(id).await?;
    let analysis = tokio::task::spawn_blocking(move || {
        // A decoder panicking on a malformed file costs that track only.
        std::panic::catch_unwind(|| crate::player::waveform::analyse_bytes(bytes, duration_ms))
            .ok()
            .flatten()
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| crate::error::AppError::Other("could not decode the track".into()))?;
    let json = serde_json::to_string(&analysis)
        .map_err(|e| crate::error::AppError::Other(format!("cannot store the analysis: {e}")))?;
    store.put_analysis(id, &json)?;
    Ok(crate::sonic::has_sound(&analysis))
}

/// One walk through the library. Returns early when switched off or signed
/// out.
async fn pass(
    app: &AppHandle,
    session: &SharedSession,
    store: &Arc<Store>,
    handle: &ScanHandle,
    failures: &mut std::collections::HashMap<String, usize>,
) {
    let Some(client) = session.read().await.clone() else {
        return;
    };
    publish(app, handle, |s| {
        s.running = true;
        s.last_error = None;
    });
    let mut start = 0u32;
    let mut analysed = 0u64;
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !handle.enabled.load(Ordering::Relaxed) || !same_session(session, &client).await {
            break;
        }
        let page = match client.songs(start, PAGE).await {
            Ok(page) => page,
            Err(e) => {
                publish(app, handle, |s| s.last_error = Some(e.to_string()));
                break;
            }
        };
        let total = page.total.max(0) as u64;
        if page.items.is_empty() {
            break;
        }
        start += page.items.len() as u32;
        let mut todo = Vec::new();
        for track in page.items {
            if !seen.insert(track.id.clone()) {
                continue;
            }
            if needs_analysis(store, &track.id, track.duration_ms) {
                if failures.get(&track.id).copied().unwrap_or(0) < MAX_FAILURES {
                    todo.push(track);
                }
            } else {
                analysed += 1;
            }
        }
        publish(app, handle, |s| {
            s.total = Some(total);
            s.analysed = analysed;
        });

        for chunk in todo.chunks(PARALLEL) {
            // Signed out, another server, or a re-login: this pass's client
            // is not the session any more. The next pass starts with the
            // new one (`install_session` pokes).
            if !handle.enabled.load(Ordering::Relaxed) || !same_session(session, &client).await {
                break;
            }
            publish(app, handle, |s| {
                s.current = chunk.first().map(|t| format!("{} — {}", t.artist, t.name));
            });
            let mut jobs = tokio::task::JoinSet::new();
            for track in chunk {
                let client = client.clone();
                let store = store.clone();
                let id = track.id.clone();
                let duration_ms = track.duration_ms;
                jobs.spawn(async move {
                    let result = analyse_one(&client, &store, &id, duration_ms).await;
                    (id, result)
                });
            }
            let mut results = Vec::new();
            while let Some(joined) = jobs.join_next().await {
                if let Ok(result) = joined {
                    results.push(result);
                }
            }
            let mut failed = None;
            for (id, result) in results {
                match result {
                    Ok(true) => analysed += 1,
                    // Analysed, but too short or too quiet to have a sound
                    // vector: stored as such, done, nothing to compare.
                    Ok(false) => analysed += 1,
                    Err(e) => {
                        tracing::debug!("sonic analysis of {id} failed: {e}");
                        *failures.entry(id).or_default() += 1;
                        failed = Some(e.to_string());
                    }
                }
            }
            publish(app, handle, |s| {
                s.analysed = analysed;
                if let Some(e) = &failed {
                    s.last_error = Some(e.clone());
                }
            });
            if failed.is_some() {
                tokio::time::sleep(ERROR_BACKOFF).await;
            }
        }
        if u64::from(start) >= total {
            break;
        }
    }
    publish(app, handle, |s| {
        s.running = false;
        s.current = None;
    });
}
