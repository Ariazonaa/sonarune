//! Background analysis of the whole library, for sonic similarity.
//!
//! The player analyses the tracks it plays from a whole file anyway
//! (`player/waveform.rs`: a direct stream played from its start, never a live
//! transcode); this fills in the rest, so a radio or a path can reach tracks
//! that were never played that way on this machine. Off until the user
//! switches it on. A pass lists the library first — what is analysed, what is
//! left — and then pulls each remaining track as a small file
//! (`JellyfinClient::analysis_audio`), two at a time, so a home server does
//! not have to transcode more than that at once. Everything is resumable: a
//! track counts as done the moment its analysis is in the store, and a
//! restart picks up from there.
//!
//! The listing also keeps the store's idea of which analyses belong to the
//! server in step with its library: one tagged for another server, or for
//! none, is claimed for it, one of a track the library no longer lists is
//! released (`Store::claim_analyses`, `Store::release_analyses`). A server is
//! its Jellyfin id, not the address it is reached at (`identity.rs`): without
//! a listing — the scan switched off — what was analysed under another
//! address of the same server is its own all the same, and every sign-in
//! settles it (`identity::settle`).

use crate::api::{JellyfinClient, LibrarySignature};
use crate::error::AppError;
use crate::identity::{Owner, Server};
use crate::player::analysis::TrackAnalysis;
use crate::player::SharedSession;
use crate::store::{keys, Store};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

const PAGE: u32 = 500;
const PARALLEL: usize = 2;
/// How often an idle scan looks for new music. Looking is one small request
/// (the library signature); the library is only walked again when that
/// changed or the last full walk is older than [`FULL_RECHECK`]. A change
/// `library_watch` notices and a sign-in poke the scan at once
/// (`ScanHandle::poke`).
const IDLE_RECHECK: Duration = Duration::from_secs(30 * 60);
/// A file replaced in place changes neither the newest date nor the count
/// the library signature is made of; a full walk this often still finds it.
const FULL_RECHECK: Duration = Duration::from_secs(24 * 60 * 60);
/// After a failed download: wait before the next track, so a server that
/// is down does not get hammered.
const ERROR_BACKOFF: Duration = Duration::from_secs(20);
/// Tracks that failed this many times in one session are skipped until the
/// next start.
const MAX_FAILURES: usize = 2;
/// Downloads in a row where every track failed: that is the server (a broken
/// transcoder, a full disk), not the tracks, and the pass stops instead of
/// striking its way through the rest of the library.
const FAILED_CHUNKS_IN_A_ROW: usize = 3;
/// How often the scan itself renews a revoked session. A renewal pokes the
/// next pass at once (`install_session`), so a server that refused every
/// fresh session as well would otherwise keep the scan logging in.
const RENEWAL_SPACING: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatus {
    pub enabled: bool,
    /// A pass is going through the library right now.
    pub running: bool,
    /// Tracks of this library the current sound analysis is done with, out
    /// of `total`: what the last complete listing found, counted on as the
    /// pass analyses the rest. Includes the few too short or too quiet to
    /// have a sound to compare.
    pub analysed: u64,
    /// Tracks of this library the analysis covers — all but the ones longer
    /// than it fetches (`JellyfinClient::MAX_ANALYSIS_MS`). `None` until a
    /// listing of this library is known.
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

    /// Walk the library now, whatever its signature says (the library
    /// changed, signed in, switched on).
    pub fn poke(&self) {
        self.notify.notify_one();
    }
}

/// Server and user id: whose library a listing describes. Accounts on one
/// server can see different libraries.
type Identity = Owner;

fn identity(client: &JellyfinClient) -> Identity {
    Owner {
        server: Server::of(client),
        user: client.user_id().unwrap_or_default().to_string(),
    }
}

/// What the last complete listing of an account's library found, kept in the
/// store ([`keys::SONIC_SCAN_PROGRESS`], a list) so Settings has a true
/// number before the first listing of a session, while the scan is off, and
/// for an account signed in again after another one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    /// The address it was listed under. The account is what counts: the
    /// same one under another address has the same library.
    server: String,
    /// The Jellyfin id of the server; absent in counts kept before it was,
    /// which go by the user id alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_id: Option<String>,
    user: String,
    analysed: u64,
    total: u64,
}

impl Progress {
    /// Whether this is the count of `identity`'s library: the same user id
    /// on the same server. A count from before server ids were kept goes by
    /// the user id; for a server whose id is not known, a count with an id
    /// is its own only under its address.
    fn is_of(&self, identity: &Identity) -> bool {
        self.user == identity.user
            && match (&identity.server.id, &self.server_id) {
                (Some(mine), Some(theirs)) => mine == theirs,
                (_, None) => true,
                (None, Some(_)) => self.server == identity.server.address,
            }
    }

    /// Whether a count of `identity`'s library saved now takes this one's
    /// place. One kept before server ids were, listed under another address,
    /// stays: it may be the count of another server the account is on.
    fn is_replaced_by(&self, identity: &Identity) -> bool {
        self.is_of(identity)
            && (self.server_id.is_some()
                || identity.server.id.is_none()
                || self.server == identity.server.address)
    }
}

/// Accounts whose counts are kept, the one listed last first.
const KEPT_PROGRESS: usize = 16;

fn saved_list(store: &Store) -> Vec<Progress> {
    let Some(json) = store.get(keys::SONIC_SCAN_PROGRESS) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<Progress>>(&json)
        // One entry on its own: how it was kept before the list.
        .or_else(|_| serde_json::from_str::<Progress>(&json).map(|one| vec![one]))
        .unwrap_or_default()
}

/// (analysed, total) of the last complete listing of the library of
/// `identity`'s account.
fn saved_progress(store: &Store, identity: &Identity) -> Option<(u64, u64)> {
    if identity.user.is_empty() {
        return None;
    }
    let list = saved_list(store);
    // The server's own count before one from before ids were kept.
    list.iter()
        .find(|p| p.is_of(identity) && p.server_id.is_some())
        .or_else(|| list.iter().find(|p| p.is_of(identity)))
        .map(|progress| (progress.analysed, progress.total))
}

fn save_progress(store: &Store, identity: &Identity, analysed: u64, total: u64) {
    if identity.user.is_empty() {
        return;
    }
    let mut list = saved_list(store);
    list.retain(|progress| !progress.is_replaced_by(identity));
    list.insert(
        0,
        Progress {
            server: identity.server.address.clone(),
            server_id: identity.server.id.clone(),
            user: identity.user.clone(),
            analysed,
            total,
        },
    );
    list.truncate(KEPT_PROGRESS);
    if let Ok(json) = serde_json::to_string(&list) {
        let _ = store.set(keys::SONIC_SCAN_PROGRESS, &json);
    }
}

/// What a pass has to do about one listed track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Todo {
    /// Fetch and analyse it.
    Analyse,
    /// Analysed, but tagged with another server or none, or with this one
    /// under another address: claim it for this one, no download needed.
    Claim,
    /// Analysed, for this server.
    Done,
}

fn todo_for(store: &Store, server: &Server, item_id: &str, duration_ms: u64) -> Todo {
    let Some(row) = store.analysis_row(item_id) else {
        return Todo::Analyse;
    };
    match serde_json::from_str::<TrackAnalysis>(&row.json) {
        Ok(analysis)
            if analysis.version == crate::player::analysis::VERSION
                && crate::sonic::sound_attempted(&analysis)
                // Not a file replaced on the server since.
                && analysis.fits(duration_ms) =>
        {
            if row.is_tagged_for(server) {
                Todo::Done
            } else {
                Todo::Claim
            }
        }
        _ => Todo::Analyse,
    }
}

/// Whether a failed fetch says something about this one track — it cannot be
/// fetched, decoded or stored as it is — rather than about the server. A
/// server that cannot be reached, no longer takes the session or is
/// overloaded fails every track alike, and a strike for each of them would
/// shelve healthy tracks until the next start.
fn about_the_track(error: &AppError) -> bool {
    match error {
        AppError::Network(e) => !e.is_connect(),
        AppError::Server { status, .. } => {
            !matches!(status, 401 | 403 | 408 | 429 | 502 | 503 | 504)
        }
        AppError::NotConnected | AppError::Db(_) => false,
        _ => true,
    }
}

/// Whether the scan may renew the session now, having last done so at
/// `last` ([`RENEWAL_SPACING`]).
fn renewal_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= RENEWAL_SPACING)
}

/// The last walk that listed the whole library and left nothing to retry.
struct Walk {
    identity: Identity,
    signature: LibrarySignature,
    at: Instant,
}

/// Whether this look has to walk the library: when poked, when nothing is
/// known of an earlier walk of it that left nothing to do, when the library
/// changed since (or its signature could not be had), or when that walk is
/// [`FULL_RECHECK`] old.
fn walk_needed(
    poked: bool,
    last: Option<&Walk>,
    identity: &Identity,
    signature: Option<&LibrarySignature>,
    now: Instant,
) -> bool {
    if poked {
        return true;
    }
    let (Some(walk), Some(signature)) = (last, signature) else {
        return true;
    };
    walk.identity != *identity
        || walk.signature != *signature
        || now.saturating_duration_since(walk.at) >= FULL_RECHECK
}

/// A listed track the pass still has to analyse.
struct Pending {
    id: String,
    /// "Artist — Title", for the status line.
    label: String,
    duration_ms: u64,
}

/// A listing pages by offset over a library that can change under it: an item
/// removed before the offset shifts later ones past it, unseen, and they would
/// be released as gone. Only a library that looks the same after the listing
/// as before it counts as listed whole; otherwise nothing is released, and the
/// next look walks again.
fn settled(
    listed: Listed,
    before: Option<&LibrarySignature>,
    after: Option<&LibrarySignature>,
) -> Listed {
    match (listed, before, after) {
        (Listed::Whole, Some(before), Some(after)) if before == after => Listed::Whole,
        (Listed::Whole, _, _) => Listed::Part,
        (other, _, _) => other,
    }
}

/// How the listing of a pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listed {
    /// The whole library.
    Whole,
    /// Part of it: a page failed, or came back empty short of the total.
    /// What it found is still analysed, but nothing is released or counted
    /// on the strength of it.
    Part,
    /// Switched off, or the session is not this pass's any more.
    Stopped,
}

/// (analysed, total) a pass counts on from after its listing: what the
/// listing found when it got through the whole library (`done` of the
/// `covered` tracks). After part of it, the counts shown so far — the last
/// complete listing's — unless the part already found more: with nothing
/// complete to go by, what was listed is a lower bound.
fn counts_after(listed: Listed, done: u64, covered: u64, shown: (u64, Option<u64>)) -> (u64, u64) {
    match (listed, shown) {
        (Listed::Whole, _) | (_, (_, None)) => (done, covered),
        (_, (analysed, Some(total))) => {
            let total = total.max(covered);
            (analysed.max(done).min(total), total)
        }
    }
}

pub fn spawn(app: AppHandle, session: SharedSession, store: Arc<Store>) -> ScanHandle {
    let enabled = store.get(keys::SONIC_SCAN).as_deref() == Some("true");
    // Before the first listing of the session: what the last one found for
    // the account signed in.
    let signed_in = Owner::current(&store);
    let saved = signed_in
        .as_ref()
        .and_then(|identity| saved_progress(&store, identity));
    let handle = ScanHandle {
        status: Arc::new(Mutex::new(ScanStatus {
            enabled,
            analysed: saved.map_or(0, |(analysed, _)| analysed),
            total: saved.map(|(_, total)| total),
            ..ScanStatus::default()
        })),
        enabled: Arc::new(AtomicBool::new(enabled)),
        notify: Arc::new(Notify::new()),
    };
    let scan = Scan {
        app,
        session,
        store,
        handle: handle.clone(),
        failures: HashMap::new(),
        last_walk: None,
        shown_for: signed_in,
        renewed_at: None,
    };
    tauri::async_runtime::spawn(scan.run());
    handle
}

/// The scan task and what it keeps between passes.
struct Scan {
    app: AppHandle,
    session: SharedSession,
    store: Arc<Store>,
    handle: ScanHandle,
    /// Tracks that failed this session, and how often.
    failures: HashMap<String, usize>,
    last_walk: Option<Walk>,
    /// Whose library the counts in the status describe.
    shown_for: Option<Identity>,
    /// When the scan last renewed a revoked session ([`Scan::renew`]).
    renewed_at: Option<Instant>,
}

impl Scan {
    async fn run(mut self) {
        // The first round walks; after that a poke does, or a look that
        // finds the library changed.
        let mut poked = true;
        loop {
            let client = self.session.read().await.clone();
            if let Some(client) = client {
                let me = identity(&client);
                if self.handle.enabled.load(Ordering::Relaxed) {
                    self.pass(client, poked).await;
                } else {
                    // Off: still say how far the analysis of this library got.
                    self.show_saved(&me);
                }
            }
            poked = tokio::select! {
                _ = self.handle.notify.notified() => true,
                _ = tokio::time::sleep(IDLE_RECHECK) => false,
            };
        }
    }

    fn publish(&self, update: impl FnOnce(&mut ScanStatus)) {
        let snapshot = {
            let mut status = self.handle.status.lock().unwrap();
            update(&mut status);
            status.clone()
        };
        let _ = self.app.emit("sonic:scan", snapshot);
    }

    /// The counts of the last complete listing of `identity`'s library, or
    /// none, when the status shows another library's.
    fn show_saved(&mut self, identity: &Identity) {
        if self.shown_for.as_ref() == Some(identity) {
            return;
        }
        let saved = saved_progress(&self.store, identity);
        self.publish(|s| {
            s.analysed = saved.map_or(0, |(analysed, _)| analysed);
            s.total = saved.map(|(_, total)| total);
        });
        self.shown_for = Some(identity.clone());
    }

    /// A 401 in this pass: renew the session the way every caller renews it
    /// (`commands::renew_session`), at most every [`RENEWAL_SPACING`]. The
    /// pass ends either way — `client` is not the session any more, or still
    /// rejected — and a renewed session pokes the next one, which is the
    /// retry.
    async fn renew(&mut self, client: &Arc<JellyfinClient>, unauthorized: AppError) {
        let now = Instant::now();
        if !renewal_due(self.renewed_at, now) {
            return;
        }
        self.renewed_at = Some(now);
        if let Err(e) = crate::commands::renew_session(&self.app, client, now, unauthorized).await {
            tracing::debug!("the sonic scan could not renew the session: {e}");
        }
    }

    /// Switched on, and `client` still the session: a pass goes on.
    async fn still_on(&self, client: &Arc<JellyfinClient>) -> bool {
        self.handle.enabled.load(Ordering::Relaxed)
            && self
                .session
                .read()
                .await
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, client))
    }

    /// One look at the library: a walk when poked, when it changed, or when
    /// the last full walk is a day old. Returns early when switched off or
    /// signed out — or another server, or a re-login: this pass's client is
    /// not the session any more, and the next pass starts with the new one
    /// (`install_session` pokes).
    async fn pass(&mut self, client: Arc<JellyfinClient>, poked: bool) {
        let me = identity(&client);
        // What the library looks like now; a change while it is walked shows
        // up at the next look.
        let signature = client.library_signature().await.ok();
        if !walk_needed(
            poked,
            self.last_walk.as_ref(),
            &me,
            signature.as_ref(),
            Instant::now(),
        ) {
            return;
        }
        self.show_saved(&me);
        self.publish(|s| {
            s.running = true;
            s.last_error = None;
        });
        let clean = self.walk(&client, &me, signature.as_ref()).await;
        self.last_walk = match (clean, signature) {
            (true, Some(signature)) => Some(Walk {
                identity: me,
                signature,
                at: Instant::now(),
            }),
            _ => None,
        };
        self.publish(|s| {
            s.running = false;
            s.current = None;
        });
    }

    /// List the library, then analyse what is left — of all of it, or of
    /// the part listed before a page failed. True when it got through all of
    /// it with nothing failing.
    async fn walk(
        &mut self,
        client: &Arc<JellyfinClient>,
        me: &Identity,
        before: Option<&LibrarySignature>,
    ) -> bool {
        let server = &me.server;
        // The rows of this server before the listing: what it does not list
        // any more is released afterwards. Analyses written meanwhile are
        // not in here, so a track played during the walk keeps its own.
        let owned = self.store.analysis_ids_of(server).unwrap_or_default();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending: Vec<Pending> = Vec::new();
        let (mut done, mut too_long) = (0u64, 0u64);
        let mut start = 0u32;
        let listed = loop {
            if !self.still_on(client).await {
                break Listed::Stopped;
            }
            let page = match client.songs(start, PAGE).await {
                Ok(page) => page,
                Err(e) => {
                    self.publish(|s| s.last_error = Some(e.to_string()));
                    if matches!(e, AppError::Server { status: 401, .. }) {
                        self.renew(client, e).await;
                    }
                    break Listed::Part;
                }
            };
            let total = page.total.max(0) as u64;
            if page.items.is_empty() {
                // Short of the total, an empty page is not the end of the
                // library: nothing may be released on the strength of it.
                break if u64::from(start) >= total {
                    Listed::Whole
                } else {
                    Listed::Part
                };
            }
            start += page.items.len() as u32;
            let mut claims = Vec::new();
            for track in page.items {
                if !seen.insert(track.id.clone()) {
                    continue;
                }
                match todo_for(&self.store, server, &track.id, track.duration_ms) {
                    Todo::Done => done += 1,
                    Todo::Claim => {
                        done += 1;
                        claims.push(track.id);
                    }
                    // Longer than the analysis fetches: a mix, not a track
                    // to compare. Not fetched, and not counted either.
                    Todo::Analyse if track.duration_ms > JellyfinClient::MAX_ANALYSIS_MS => {
                        too_long += 1;
                    }
                    Todo::Analyse => {
                        if self.failures.get(&track.id).copied().unwrap_or(0) < MAX_FAILURES {
                            pending.push(Pending {
                                label: format!("{} — {}", track.artist, track.name),
                                id: track.id,
                                duration_ms: track.duration_ms,
                            });
                        }
                    }
                }
            }
            if let Err(e) = self.store.claim_analyses(server, &claims) {
                tracing::warn!("cannot claim the analyses of this library: {e}");
            }
            if u64::from(start) >= total {
                break Listed::Whole;
            }
        };
        if listed == Listed::Stopped {
            return false;
        }
        let listed = if listed == Listed::Whole {
            let after = client.library_signature().await.ok();
            settled(listed, before, after.as_ref())
        } else {
            listed
        };
        let whole = listed == Listed::Whole;
        if whole {
            let gone: Vec<String> = owned.into_iter().filter(|id| !seen.contains(id)).collect();
            match self.store.release_analyses(server, &gone) {
                Ok(0) => {}
                Ok(released) => tracing::debug!("{released} analyses no longer in the library"),
                Err(e) => tracing::warn!("cannot release the analyses of gone tracks: {e}"),
            }
        }
        let shown = {
            let status = self.handle.status.lock().unwrap();
            (status.analysed, status.total)
        };
        let (mut analysed, total) = counts_after(listed, done, seen.len() as u64 - too_long, shown);
        self.publish(|s| {
            s.total = Some(total);
            s.analysed = analysed;
        });
        self.shown_for = Some(me.clone());
        if whole {
            save_progress(&self.store, me, analysed, total);
        }

        let mut clean = true;
        let mut failed_in_a_row = 0;
        for chunk in pending.chunks(PARALLEL) {
            if !self.still_on(client).await {
                clean = false;
                break;
            }
            self.publish(|s| s.current = chunk.first().map(|t| t.label.clone()));
            let mut jobs = tokio::task::JoinSet::new();
            for track in chunk {
                let client = client.clone();
                let store = self.store.clone();
                let (id, duration_ms) = (track.id.clone(), track.duration_ms);
                jobs.spawn(async move {
                    let result = analyse_one(&client, &store, &id, duration_ms).await;
                    (id, result)
                });
            }
            let (mut struck, mut server_trouble) = (0, false);
            let (mut failed, mut unauthorized) = (None, None);
            while let Some(joined) = jobs.join_next().await {
                let Ok((id, result)) = joined else {
                    continue;
                };
                match result {
                    // With a sound vector or without one (too short or too
                    // quiet to compare): stored as such, done.
                    Ok(_) => analysed += 1,
                    Err(e) => {
                        tracing::debug!("sonic analysis of {id} failed: {e}");
                        if about_the_track(&e) {
                            *self.failures.entry(id).or_default() += 1;
                            struck += 1;
                        } else {
                            server_trouble = true;
                        }
                        failed = Some(e.to_string());
                        if matches!(e, AppError::Server { status: 401, .. }) {
                            unauthorized = Some(e);
                        }
                    }
                }
            }
            // After part of a listing the count goes on from the last
            // complete one, which may already have counted a track.
            analysed = analysed.min(total);
            self.publish(|s| {
                s.analysed = analysed;
                if let Some(e) = &failed {
                    s.last_error = Some(e.clone());
                }
            });
            if failed.is_some() {
                clean = false;
            }
            // Not the tracks' fault: the next look tries them again.
            if server_trouble {
                if let Some(unauthorized) = unauthorized {
                    self.renew(client, unauthorized).await;
                }
                break;
            }
            failed_in_a_row = if struck == chunk.len() {
                failed_in_a_row + 1
            } else {
                0
            };
            if failed_in_a_row >= FAILED_CHUNKS_IN_A_ROW {
                break;
            }
            if struck > 0 {
                tokio::time::sleep(ERROR_BACKOFF).await;
            }
        }
        if whole {
            save_progress(&self.store, me, analysed, total);
        }
        clean && whole
    }
}

/// Fetch, analyse and store one track. `Ok(false)`: analysed, but it has no
/// sound vector (too short or too quiet to compare).
async fn analyse_one(
    client: &JellyfinClient,
    store: &Store,
    id: &str,
    duration_ms: u64,
) -> crate::error::AppResult<bool> {
    let bytes = client.analysis_audio(id, duration_ms).await?;
    let analysis = tokio::task::spawn_blocking(move || {
        // A decoder panicking on a malformed file costs that track only.
        std::panic::catch_unwind(|| crate::player::waveform::analyse_bytes(bytes, duration_ms))
            .ok()
            .flatten()
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| AppError::Other("could not decode the track".into()))?;
    let json = serde_json::to_string(&analysis)
        .map_err(|e| AppError::Other(format!("cannot store the analysis: {e}")))?;
    // Filed under the server the audio came from, even when the session has
    // moved on while it downloaded.
    store.put_analysis_from(Some(&Server::of(client)), id, &json)?;
    Ok(crate::sonic::has_sound(&analysis))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::analysis::VERSION;
    use crate::player::timbre::{SonicFeatures, SONIC_VERSION};
    use std::path::PathBuf;

    /// A store in its own temp directory, closed before the directory goes:
    /// Windows refuses to delete an open database file.
    struct TempStore {
        store: Option<Store>,
        dir: PathBuf,
    }

    impl std::ops::Deref for TempStore {
        type Target = Store;
        fn deref(&self) -> &Store {
            self.store.as_ref().expect("open until drop")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            self.store.take();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_store() -> TempStore {
        let dir = std::env::temp_dir().join(format!("sonarune-scan-test-{}", uuid::Uuid::new_v4()));
        TempStore {
            store: Some(Store::open(&dir).expect("open test store")),
            dir,
        }
    }

    const SERVER: &str = "https://music.home";

    /// A server known by its address alone, as before ids were kept.
    fn at(address: &str) -> Server {
        Server {
            address: address.into(),
            id: None,
        }
    }

    fn owner(server: Server, user: &str) -> Identity {
        Owner {
            server,
            user: user.into(),
        }
    }

    fn stored_with(
        store: &Store,
        server: Option<&Server>,
        id: &str,
        decoded_ms: f64,
        server_ms: Option<u64>,
        sonic: SonicFeatures,
    ) {
        let analysis = TrackAnalysis {
            version: VERSION,
            bpm: Some(120.0),
            tempo_confidence: 0.5,
            intro: None,
            outro: None,
            key: None,
            key_correlation: 0.0,
            audible_start_ms: 0.0,
            audible_end_ms: decoded_ms,
            duration_ms: decoded_ms,
            server_duration_ms: server_ms,
            sonic: Some(sonic),
        };
        store
            .put_analysis_from(server, id, &serde_json::to_string(&analysis).unwrap())
            .unwrap();
    }

    /// An analysis stored under the address `server` (no id), or nobody's.
    fn stored(
        store: &Store,
        server: Option<&str>,
        id: &str,
        decoded_ms: f64,
        server_ms: Option<u64>,
        sonic: u32,
    ) {
        let features = SonicFeatures {
            version: sonic,
            vector: vec![0.5; crate::player::timbre::DIMENSIONS],
        };
        stored_with(
            store,
            server.map(at).as_ref(),
            id,
            decoded_ms,
            server_ms,
            features,
        );
    }

    /// A row of `user`'s play history: `item` played under `server`.
    fn played(store: &Store, server: &str, user: &str, item: &str) {
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO play_history
                         (server_url, user_id, played_at, listened_ms, item_id, track)
                     VALUES (?1, ?2, 0, 0, ?3, '{}')",
                    rusqlite::params![server, user, item],
                )
            })
            .unwrap();
    }

    fn tag(store: &Store, id: &str) -> Option<String> {
        store.analysis_row(id).and_then(|row| row.server_url)
    }

    #[test]
    fn a_listed_track_is_analysed_claimed_or_left_alone() {
        let store = temp_store();
        assert_eq!(todo_for(&store, &at(SERVER), "new", 200_000), Todo::Analyse);
        stored(
            &store,
            Some(SERVER),
            "done",
            200_000.0,
            Some(200_000),
            SONIC_VERSION,
        );
        assert_eq!(todo_for(&store, &at(SERVER), "done", 200_000), Todo::Done);
        // Analysed under another address of the server, or before analyses
        // recorded one: no download, only a claim.
        stored(
            &store,
            Some("http://10.0.0.5:8096"),
            "moved",
            200_000.0,
            Some(200_000),
            SONIC_VERSION,
        );
        assert_eq!(todo_for(&store, &at(SERVER), "moved", 200_000), Todo::Claim);
        stored(
            &store,
            None,
            "legacy",
            200_000.0,
            Some(200_000),
            SONIC_VERSION,
        );
        assert_eq!(
            todo_for(&store, &at(SERVER), "legacy", 200_000),
            Todo::Claim
        );
        // The sound analysis changed since.
        stored(
            &store,
            Some(SERVER),
            "old",
            200_000.0,
            Some(200_000),
            SONIC_VERSION - 1,
        );
        assert_eq!(todo_for(&store, &at(SERVER), "old", 200_000), Todo::Analyse);
        // A file replaced on the server: another length.
        assert_eq!(
            todo_for(&store, &at(SERVER), "done", 260_000),
            Todo::Analyse
        );
    }

    #[test]
    fn a_track_of_this_server_under_another_address_is_claimed_not_fetched() {
        let store = temp_store();
        let lan = "http://10.0.0.5:8096";
        let with = |address: &str, id: &str| Server {
            address: address.into(),
            id: Some(id.into()),
        };
        let current = || SonicFeatures {
            version: SONIC_VERSION,
            vector: vec![0.5; crate::player::timbre::DIMENSIONS],
        };
        let me = with(SERVER, "s1");
        stored_with(
            &store,
            Some(&with(lan, "s1")),
            "moved",
            200_000.0,
            Some(200_000),
            current(),
        );
        stored_with(
            &store,
            Some(&me),
            "done",
            200_000.0,
            Some(200_000),
            current(),
        );
        assert_eq!(todo_for(&store, &me, "moved", 200_000), Todo::Claim);
        assert_eq!(todo_for(&store, &me, "done", 200_000), Todo::Done);
        // Listed by another server at the same address: its own now.
        assert_eq!(
            todo_for(&store, &with(SERVER, "copy"), "done", 200_000),
            Todo::Claim
        );
        // Its id not known: the address decides, as before ids were kept.
        assert_eq!(todo_for(&store, &at(SERVER), "done", 200_000), Todo::Done);
    }

    #[test]
    fn a_decode_longer_than_the_server_says_is_not_fetched_again() {
        // A VBR MP3 whose length the server estimated: it decodes 9 s longer,
        // every time. Analysed once, it stays analysed.
        let store = temp_store();
        stored(
            &store,
            Some(SERVER),
            "vbr",
            247_000.0,
            Some(238_000),
            SONIC_VERSION,
        );
        assert_eq!(todo_for(&store, &at(SERVER), "vbr", 238_000), Todo::Done);
        // Stored before the server length was: the decode is all there is to
        // go by, so it is analysed once more — and then stays.
        stored(
            &store,
            Some(SERVER),
            "older",
            247_000.0,
            None,
            SONIC_VERSION,
        );
        assert_eq!(
            todo_for(&store, &at(SERVER), "older", 238_000),
            Todo::Analyse
        );
    }

    #[test]
    fn a_library_that_changed_during_the_listing_is_not_listed_whole() {
        let at = |count| LibrarySignature {
            newest: "2026-10-20T12:00:00Z".into(),
            count,
        };
        assert_eq!(
            settled(Listed::Whole, Some(&at(10)), Some(&at(10))),
            Listed::Whole
        );
        // Something left or came meanwhile, or nobody answered: nothing may
        // be released on the strength of that listing.
        assert_eq!(
            settled(Listed::Whole, Some(&at(10)), Some(&at(9))),
            Listed::Part
        );
        assert_eq!(settled(Listed::Whole, Some(&at(10)), None), Listed::Part);
        assert_eq!(settled(Listed::Whole, None, Some(&at(10))), Listed::Part);
        assert_eq!(
            settled(Listed::Part, Some(&at(10)), Some(&at(10))),
            Listed::Part
        );
    }

    #[test]
    fn the_scan_renews_a_session_at_most_every_so_often() {
        let now = Instant::now();
        assert!(renewal_due(None, now));
        assert!(!renewal_due(Some(now), now));
        assert!(!renewal_due(Some(now), now + RENEWAL_SPACING / 2));
        assert!(renewal_due(Some(now), now + RENEWAL_SPACING));
    }

    #[test]
    fn only_trouble_with_one_track_counts_against_it() {
        let server = |status| AppError::Server {
            status,
            message: String::new(),
        };
        for status in [400, 404, 415, 500] {
            assert!(about_the_track(&server(status)), "{status}");
        }
        for status in [401, 403, 429, 502, 503, 504] {
            assert!(!about_the_track(&server(status)), "{status}");
        }
        assert!(about_the_track(&AppError::Other(
            "could not decode the track".into()
        )));
        assert!(!about_the_track(&AppError::NotConnected));
    }

    #[test]
    fn an_idle_look_walks_only_when_something_may_have_changed() {
        let me = owner(at(SERVER), "u1");
        let signature = |count| LibrarySignature {
            newest: "2026-10-01T00:00:00Z".into(),
            count,
        };
        let walk = Walk {
            identity: me.clone(),
            signature: signature(20_000),
            at: Instant::now(),
        };
        let now = walk.at;
        assert!(!walk_needed(
            false,
            Some(&walk),
            &me,
            Some(&signature(20_000)),
            now
        ));
        assert!(
            walk_needed(true, Some(&walk), &me, Some(&signature(20_000)), now),
            "poked"
        );
        assert!(
            walk_needed(false, Some(&walk), &me, Some(&signature(20_012)), now),
            "changed"
        );
        assert!(
            walk_needed(false, Some(&walk), &me, None, now),
            "no signature"
        );
        assert!(
            walk_needed(false, None, &me, Some(&signature(20_000)), now),
            "no clean walk"
        );
        let other = owner(at(SERVER), "u2");
        assert!(walk_needed(
            false,
            Some(&walk),
            &other,
            Some(&signature(20_000)),
            now
        ));
        let tomorrow = now + FULL_RECHECK;
        assert!(walk_needed(
            false,
            Some(&walk),
            &me,
            Some(&signature(20_000)),
            tomorrow
        ));
    }

    #[test]
    fn the_last_count_is_kept_per_account() {
        let store = temp_store();
        let me = owner(at(SERVER), "u1");
        assert_eq!(saved_progress(&store, &me), None);
        save_progress(&store, &me, 19_990, 20_000);
        assert_eq!(saved_progress(&store, &me), Some((19_990, 20_000)));
        // Another account is signed in and listed; the first one keeps its
        // count for when it comes back.
        let kid = owner(at(SERVER), "kid");
        assert_eq!(saved_progress(&store, &kid), None);
        save_progress(&store, &kid, 300, 400);
        assert_eq!(saved_progress(&store, &kid), Some((300, 400)));
        assert_eq!(saved_progress(&store, &me), Some((19_990, 20_000)));
        // The same account under another address of its server: the same
        // library.
        assert_eq!(
            saved_progress(&store, &owner(at("https://music.example.org"), "u1")),
            Some((19_990, 20_000))
        );
        save_progress(&store, &me, 20_000, 20_000);
        assert_eq!(saved_progress(&store, &me), Some((20_000, 20_000)));
        for i in 0..KEPT_PROGRESS {
            save_progress(&store, &owner(at(SERVER), &format!("other{i}")), 1, 1);
        }
        assert_eq!(saved_progress(&store, &me), None, "the oldest went");
        // One entry on its own, as it was kept before.
        store
            .set(
                keys::SONIC_SCAN_PROGRESS,
                r#"{"server":"https://music.home","user":"u1","analysed":5,"total":9}"#,
            )
            .unwrap();
        assert_eq!(saved_progress(&store, &me), Some((5, 9)));
        assert_eq!(saved_progress(&store, &owner(at(SERVER), "")), None);
    }

    #[test]
    fn the_count_of_one_server_is_not_shown_for_another() {
        let store = temp_store();
        let server = |address: &str, id: &str| Server {
            address: address.into(),
            id: Some(id.into()),
        };
        let first = owner(server(SERVER, "first"), "u1");
        // A copy of the server: the same user ids, an id of its own.
        let copy = owner(server("https://copy.home", "copy"), "u1");
        save_progress(&store, &first, 19_990, 20_000);
        assert_eq!(saved_progress(&store, &copy), None);
        save_progress(&store, &copy, 300, 400);
        assert_eq!(saved_progress(&store, &first), Some((19_990, 20_000)));
        assert_eq!(saved_progress(&store, &copy), Some((300, 400)));
        // The first one under another address: the same library.
        let proxied = owner(server("https://music.example.org", "first"), "u1");
        assert_eq!(saved_progress(&store, &proxied), Some((19_990, 20_000)));
        // Its id not known: its own count only at its address.
        assert_eq!(
            saved_progress(&store, &owner(at(SERVER), "u1")),
            Some((19_990, 20_000))
        );
        assert_eq!(
            saved_progress(&store, &owner(at("https://elsewhere.home"), "u1")),
            None
        );

        // A count kept by 0.6.1, without the server id: the account's until
        // a listing of its server replaces it.
        store
            .set(
                keys::SONIC_SCAN_PROGRESS,
                r#"[{"server":"https://music.home","user":"u2","analysed":5,"total":9}]"#,
            )
            .unwrap();
        let u2 = owner(server(SERVER, "first"), "u2");
        assert_eq!(saved_progress(&store, &u2), Some((5, 9)));
        save_progress(&store, &u2, 7, 9);
        assert_eq!(saved_list(&store).len(), 1);
        assert_eq!(saved_progress(&store, &u2), Some((7, 9)));
        // One listed elsewhere may be another server's: it stays for that
        // one until it saves its own.
        let elsewhere = r#"{"server":"https://copy.home","user":"u2","analysed":1,"total":4}"#;
        let mut list: Vec<serde_json::Value> =
            serde_json::from_str(&store.get(keys::SONIC_SCAN_PROGRESS).unwrap()).unwrap();
        list.push(serde_json::from_str(elsewhere).unwrap());
        store
            .set(
                keys::SONIC_SCAN_PROGRESS,
                &serde_json::to_string(&list).unwrap(),
            )
            .unwrap();
        save_progress(&store, &u2, 8, 9);
        assert_eq!(saved_progress(&store, &u2), Some((8, 9)));
        let copy_u2 = owner(server("https://copy.home", "copy"), "u2");
        assert_eq!(saved_progress(&store, &copy_u2), Some((1, 4)));
        save_progress(&store, &copy_u2, 2, 4);
        assert_eq!(saved_list(&store).len(), 2);

        // 0.6.1 reads the list as it did, ignoring the id.
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Progress061 {
            server: String,
            user: String,
            analysed: u64,
            total: u64,
        }
        let stored = store.get(keys::SONIC_SCAN_PROGRESS).unwrap();
        let read: Vec<Progress061> = serde_json::from_str(&stored).unwrap();
        assert_eq!((read[0].user.as_str(), read[0].analysed), ("u2", 2));
        assert!(stored.contains(r#""serverId":"first""#), "{stored}");
    }

    #[test]
    fn a_partial_listing_keeps_the_last_complete_count() {
        // The whole library listed: its own count.
        assert_eq!(
            counts_after(Listed::Whole, 19_000, 20_000, (20_000, Some(20_000))),
            (19_000, 20_000)
        );
        // A page failed on the way: the last complete count stays up.
        assert_eq!(
            counts_after(Listed::Part, 7_000, 15_500, (19_990, Some(20_000))),
            (19_990, 20_000)
        );
        // ...or, with none, what the part found: a lower bound, which the
        // next part that gets further raises.
        assert_eq!(
            counts_after(Listed::Part, 7_000, 15_500, (0, None)),
            (7_000, 15_500)
        );
        assert_eq!(
            counts_after(Listed::Part, 9_000, 18_000, (7_000, Some(15_500))),
            (9_000, 18_000)
        );
    }

    #[test]
    fn analyses_follow_the_account_to_another_address() {
        let store = temp_store();
        let old = "http://10.0.0.5:8096";
        let new = "https://music.example.org";
        played(&store, old, "u1", "a");
        stored(
            &store,
            Some(old),
            "a",
            200_000.0,
            Some(200_000),
            SONIC_VERSION,
        );
        // Analysed when it played, skipped before it counted as a listen.
        stored(
            &store,
            Some(old),
            "skipped",
            200_000.0,
            Some(200_000),
            SONIC_VERSION,
        );
        // Another server's, and one released for good.
        stored(
            &store,
            Some("https://friend.home"),
            "f",
            200_000.0,
            None,
            SONIC_VERSION,
        );
        stored(&store, Some(""), "gone", 200_000.0, None, SONIC_VERSION);
        let generation = store.analysis_generation();

        let settle = |me: Identity| crate::identity::settle(&store, &me).unwrap().0;
        // No server id known: the history ties the addresses.
        assert_eq!(settle(owner(at(new), "u1")), 2);
        assert!(store.analysis_generation() > generation);
        assert_eq!(tag(&store, "a").as_deref(), Some(new));
        assert_eq!(tag(&store, "skipped").as_deref(), Some(new));
        assert_eq!(tag(&store, "f").as_deref(), Some("https://friend.home"));
        assert_eq!(tag(&store, "gone").as_deref(), Some(""));
        // Another account has no plays tying it to the old address.
        stored(&store, Some(old), "b", 200_000.0, None, SONIC_VERSION);
        assert_eq!(settle(owner(at(new), "u2")), 0);
        assert_eq!(settle(owner(at(new), "")), 0);
        assert_eq!(tag(&store, "b").as_deref(), Some(old));
    }

    #[test]
    fn a_gone_track_is_released_only_where_no_other_account_may_see_it() {
        let store = temp_store();
        for id in ["dead", "mine", "theirs"] {
            stored(&store, Some(SERVER), id, 200_000.0, None, SONIC_VERSION);
        }
        let release = |gone: &[&str]| {
            let gone = gone.iter().map(|id| id.to_string()).collect();
            crate::sonic::release_gone(&store, &owner(at(SERVER), "kid"), gone).unwrap()
        };
        // Nobody else plays on this server: what it no longer returns is gone.
        assert_eq!(release(&["dead"]), 1);
        assert_eq!(tag(&store, "dead").as_deref(), Some(""));
        // Another account does. What the server keeps from this one may be
        // a track only the other may see; what this one has played and no
        // longer gets is gone.
        played(&store, SERVER, "admin", "theirs");
        played(&store, SERVER, "kid", "mine");
        assert_eq!(release(&["mine", "theirs"]), 1);
        assert_eq!(tag(&store, "mine").as_deref(), Some(""));
        assert_eq!(tag(&store, "theirs").as_deref(), Some(SERVER));
        assert_eq!(release(&[]), 0);
    }

    #[test]
    fn a_track_without_a_sound_is_told_from_one_not_analysed() {
        let store = temp_store();
        assert!(!crate::sonic::without_sound(&store, "new"));
        stored(&store, Some(SERVER), "loud", 200_000.0, None, SONIC_VERSION);
        assert!(!crate::sonic::without_sound(&store, "loud"));
        stored_with(
            &store,
            Some(&at(SERVER)),
            "jingle",
            2_000.0,
            None,
            SonicFeatures::none(),
        );
        assert!(crate::sonic::without_sound(&store, "jingle"));
        // Not through the current sound analysis yet: it may still get one.
        stored_with(
            &store,
            Some(&at(SERVER)),
            "older",
            2_000.0,
            None,
            SonicFeatures {
                version: SONIC_VERSION - 1,
                vector: Vec::new(),
            },
        );
        assert!(!crate::sonic::without_sound(&store, "older"));
    }

    #[test]
    fn long_tracks_come_as_the_small_transcode() {
        assert_eq!(JellyfinClient::analysis_bitrate(4 * 60_000), 320_000);
        assert_eq!(JellyfinClient::analysis_bitrate(27 * 60_000), 320_000);
        assert_eq!(JellyfinClient::analysis_bitrate(29 * 60_000), 96_000);
        assert_eq!(
            JellyfinClient::analysis_bitrate(0),
            320_000,
            "length unknown"
        );
        // The longest track fetched fits the limit as 96 kbit/s Opus, with
        // room for the container and a variable bitrate.
        let transcode = JellyfinClient::MAX_ANALYSIS_MS / 1000 * 12_000;
        assert!(transcode * 110 / 100 < JellyfinClient::MAX_ANALYSIS_BYTES);
    }
}
