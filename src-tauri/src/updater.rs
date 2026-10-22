//! In-app updates.
//!
//! An update is the ordinary NSIS installer from the GitHub release, but the
//! app runs one only after checking its **minisign signature** against the
//! public key compiled in from `tauri.conf.json` (`plugins.updater.pubkey`).
//! That is independent of Authenticode: the installer is not code-signed, so
//! Windows still reports an unknown publisher, but a download that was swapped
//! on the way — a poisoned mirror, a proxy, a tampered release asset — never
//! reaches the installer. The private key lives only in the release
//! workflow's secrets, never in this repository.
//!
//! `latest.json` on the release says which version is current and where its
//! installer and signature are; `.github/workflows/release.yml` writes it from
//! the artefacts it just built and signed.
//!
//! Two rules shape the rest:
//!
//! - **Never update while music is playing.** The installer closes the app, so
//!   an install that interrupts a track is a bug, not a trade-off. The button
//!   in Settings is disabled then, but the rule is enforced here, again right
//!   before the installer starts — a download takes long enough for playback
//!   to have begun meanwhile.
//! - **A portable copy is never turned into an installed one.** Running the
//!   NSIS installer out of a portable run would leave an installed app beside
//!   the portable exe the user keeps launching, and the update would look
//!   like it silently did nothing.
//!
//! ## Channels
//!
//! **Stable**, the default, asks `releases/latest/download/latest.json`.
//! GitHub never counts a release marked as a pre-release as the latest one,
//! so a beta does not reach anyone through it — and should one be published
//! without that mark by mistake, the stable check still turns it down: on this
//! endpoint a version with a pre-release part (`0.7.0-beta.1`) is never newer.
//!
//! **Beta** is opt-in (Settings → Updates). It asks the stable endpoint and the
//! beta endpoint, `plugins.sonarune.betaEndpoint` in `tauri.conf.json`, and
//! takes the newer answer. The beta endpoint is `beta.json` on the fixed
//! release `beta-channel`, a pointer that `.github/workflows/beta-channel.yml`
//! moves to every pre-release once it is published. Taking the newer of both
//! means a beta tester gets the stable release that follows a beta (0.7.0 is
//! newer than 0.7.0-beta.2), and a beta pointer that is missing or stale
//! holds nobody back from stable. One request more per check, and no GitHub
//! API: that one allows 60 requests an hour per address, shared by everyone
//! behind the same router.
//!
//! Leaving the beta installs nothing older — downgrades stay off — so the
//! installed beta stays until a stable release is newer than it.
//!
//! One thing to know when testing this right after cutting a release: the
//! endpoint is `releases/latest/download/latest.json`, and GitHub serves that
//! redirect from a cache. For the first minutes after publishing it can still
//! point at the release before it, so a check in that window truthfully
//! reports "up to date" — the app asked, and that is what it was told. It
//! sorts itself out on its own; there is nothing to fix here, and a
//! cache-busting parameter on every check would cost everyone a cached
//! response to spare one person a few minutes once per release.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};
use tauri_plugin_window_state::AppHandleExt;

use crate::desktop::UpdateChannel;
use crate::error::{AppError, AppResult};
use crate::player::PlaybackStatus;
use crate::AppState;

/// What can stop an update, as far as the UI needs to tell the cases apart.
///
/// The frontend sees `update:<code>` (see [`UpdateError::code`] and
/// `src/lib/updateErrors.ts`) and turns that into a sentence; the detail of a
/// failure stays in the log, where it belongs -- "the release page could not
/// be reached" is something a listener can act on, `error sending request` is
/// not.
#[derive(Debug)]
enum UpdateError {
    /// A track is playing or loading. The installer closes the app.
    Playing,
    /// Not an installed copy, so running the installer would leave two.
    Portable,
    /// Asked to install, but the endpoint offers nothing newer.
    NothingNewer,
    /// The endpoint answered, but with no release for this platform -- or with
    /// nothing that could be read as one.
    NoRelease,
    /// The endpoint could not be reached at all.
    Offline,
    /// The download broke off.
    Download,
    /// The download did not match the project's signature. Not a hiccup: the
    /// file was tampered with, came from somewhere else, or was signed with a
    /// key this build does not trust.
    Signature,
    /// The installer could not be started.
    Install,
    /// This build has no updater configured.
    Unavailable,
}

impl UpdateError {
    /// The wire form `updateErrors.ts` parses. Keep both sides in sync.
    fn code(&self) -> &'static str {
        match self {
            UpdateError::Playing => "update:playing",
            UpdateError::Portable => "update:portable",
            UpdateError::NothingNewer => "update:none",
            UpdateError::NoRelease => "update:no-release",
            UpdateError::Offline => "update:offline",
            UpdateError::Download => "update:download",
            UpdateError::Signature => "update:signature",
            UpdateError::Install => "update:install",
            UpdateError::Unavailable => "update:unavailable",
        }
    }
}

impl From<UpdateError> for AppError {
    fn from(error: UpdateError) -> Self {
        AppError::Other(error.code().into())
    }
}

/// Which step of an update failed. The plugin reports a dropped connection as
/// the same `Reqwest` error whether it was asking for `latest.json` or halfway
/// through the installer, and the two call for different advice.
#[derive(Debug, Clone, Copy)]
enum Phase {
    /// Asking the endpoint what the current release is.
    Check,
    /// The same at the beta endpoint. Only the log line differs: a beta
    /// endpoint that fails costs nothing while the stable one answers.
    BetaCheck,
    /// Fetching the installer and checking its signature.
    Download,
    /// Writing the installer out and starting it.
    Install,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Check => "update check",
            Phase::BetaCheck => "beta channel check",
            Phase::Download => "update download",
            Phase::Install => "update install",
        }
    }
}

/// Sort a plugin failure into what the user can do about it, and log what
/// actually happened -- the diagnostic export carries the log, the UI does not.
fn classify(phase: Phase, error: tauri_plugin_updater::Error) -> UpdateError {
    let (detail, error) = describe(error);
    tracing::warn!("{} failed: {detail}", phase.name());
    code_for(phase, &error)
}

/// The variants are matched by name, and the plugin's `Error` is
/// `#[non_exhaustive]`: check the arms again whenever the plugin is updated.
fn code_for(phase: Phase, error: &tauri_plugin_updater::Error) -> UpdateError {
    use tauri_plugin_updater::Error as E;
    match error {
        E::EmptyEndpoints | E::InsecureTransportProtocol => UpdateError::Unavailable,
        // A signed version that differs from the announced one is the
        // plugin's own detection of a manifest that pairs a new version
        // number with another release's file.
        E::Minisign(_)
        | E::SignatureUtf8(_)
        | E::Base64(_)
        | E::SignedVersionMismatch { .. }
        | E::MissingSignedVersion => UpdateError::Signature,
        _ => match phase {
            Phase::Check | Phase::BetaCheck => match error {
                // `latest.json` arrived but is not JSON.
                E::Reqwest(e) if e.is_decode() => UpdateError::NoRelease,
                E::Reqwest(_) | E::Network(_) => UpdateError::Offline,
                // No release for this platform, or a manifest that does not
                // read as one (a bad date, a missing url or signature).
                _ => UpdateError::NoRelease,
            },
            Phase::Download => match error {
                // The release names an installer that is not there (renamed,
                // deleted, forbidden): retrying cannot help, the releases page
                // can.
                E::Network(message)
                    if http_status(message).is_some_and(|status| (400..500).contains(&status)) =>
                {
                    UpdateError::NoRelease
                }
                // The check reached the endpoint moments ago; a failure now is
                // the transfer itself.
                _ => UpdateError::Download,
            },
            Phase::Install => UpdateError::Install,
        },
    }
}

/// The status code in the plugin's download failure, "Download request failed
/// with status: 404 Not Found".
fn http_status(message: &str) -> Option<u16> {
    let (_, rest) = message.split_once("status: ")?;
    rest.get(..3)?.parse().ok()
}

/// The log line for a failure. reqwest's own text names the URL, which makes
/// the diagnostics ring drop the whole line (`diagnostics::redact_log_line`),
/// and leaves out the cause — DNS, a refused connection, TLS, a proxy — that
/// the export exists to answer. The error is handed back for sorting.
fn describe(error: tauri_plugin_updater::Error) -> (String, tauri_plugin_updater::Error) {
    use tauri_plugin_updater::Error as E;
    match error {
        E::Reqwest(error) => {
            let error = error.without_url();
            let mut detail = error.to_string();
            let mut cause = std::error::Error::source(&error);
            while let Some(inner) = cause {
                detail.push_str(": ");
                detail.push_str(&inner.to_string());
                cause = inner.source();
            }
            (detail, E::Reqwest(error))
        }
        error => (error.to_string(), error),
    }
}

/// How long the automatic check waits after startup. The first seconds belong
/// to restoring the session and the queue; an update request would compete
/// with them for the network and delays nothing by waiting.
const STARTUP_DELAY: Duration = Duration::from_secs(20);

/// What the UI knows about updates. `version` is `None` when the endpoint has
/// nothing newer, which is the answer to "check for updates", not an error.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// The running version, so the UI can name it without a second command.
    pub current_version: String,
    pub version: Option<String>,
    /// Release notes as the manifest carries them (markdown, shown as text).
    pub notes: Option<String>,
    /// Publication date exactly as the manifest states it (RFC 3339).
    pub date: Option<String>,
    /// Whether this copy can install an update at all. False for a portable
    /// run, which may well see a newer version but must not install it (see
    /// the module docs).
    pub installable: bool,
}

/// Bytes of the running download, as `updater:progress`. `total` is missing
/// when the server sends no `Content-Length`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    downloaded: u64,
    total: Option<u64>,
}

/// Where a check asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The endpoint of `plugins.updater`: the latest stable release.
    Stable,
    /// `plugins.sonarune.betaEndpoint`: the latest published pre-release.
    Beta,
}

impl Source {
    fn phase(self) -> Phase {
        match self {
            Source::Stable => Phase::Check,
            Source::Beta => Phase::BetaCheck,
        }
    }
}

/// The channel chosen in Settings.
fn channel(app: &AppHandle) -> UpdateChannel {
    app.try_state::<AppState>()
        .map(|state| crate::desktop::load(&state.store).update_channel)
        .unwrap_or_default()
}

/// `plugins.sonarune.betaEndpoint`. Configured rather than built in, so a test
/// build can point it at a local server, as it can the stable endpoint.
fn beta_endpoint(config: &tauri::Config) -> Option<tauri::Url> {
    let endpoint = config
        .plugins
        .0
        .get("sonarune")?
        .get("betaEndpoint")?
        .as_str()?;
    tauri::Url::parse(endpoint).ok()
}

/// Whether the stable endpoint's answer counts as an update: the plugin's own
/// rule, newer than the running version, and no pre-release (see the module
/// docs).
fn stable_offer(current: &semver::Version, remote: &semver::Version) -> bool {
    remote > current && remote.pre.is_empty()
}

/// An updater for `source` whose exit hook hands the app over to the
/// installer (see [`hand_over`]).
fn updater(app: &AppHandle, source: Source) -> Result<Updater, UpdateError> {
    let handle = app.clone();
    let builder = app
        .updater_builder()
        .on_before_exit(move || hand_over(&handle));
    let builder = match source {
        Source::Stable => {
            builder.version_comparator(|current, remote| stable_offer(&current, &remote.version))
        }
        Source::Beta => {
            let Some(endpoint) = beta_endpoint(app.config()) else {
                tracing::warn!("the beta channel has no endpoint configured");
                return Err(UpdateError::Unavailable);
            };
            builder
                .endpoints(vec![endpoint])
                .map_err(|e| classify(Phase::BetaCheck, e))?
        }
    };
    builder.build().map_err(|e| classify(source.phase(), e))
}

/// Whether this process is being handed over to the installer: set by
/// [`hand_over`], cleared by [`recover_from_failed_launch`] when the launch
/// fails. A type of its own so the tests can have one without touching the
/// app's.
struct HandOverFlag(AtomicBool);

impl HandOverFlag {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn begin(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Ends a hand-over, and says whether one was under way: an install that
    /// failed before the hook (writing the installer out) took nothing away
    /// that would need giving back.
    fn end(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }

    fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

static HAND_OVER: HandOverFlag = HandOverFlag::new();

/// Whether the installer is about to end the process. The player starts no
/// track meanwhile (`Worker::start_current`): the hand-over begins with the
/// final reports, the window stays usable and media keys still reach the
/// player, and a track begun after them would be cut off a moment later, its
/// start reported to a server that never hears it stop.
pub(crate) fn handing_over() -> bool {
    HAND_OVER.is_set()
}

/// How long the hand-over waits for the main thread to save the window
/// geometry. The save itself takes milliseconds; this only bounds a main
/// thread that is busy with something else.
const WINDOW_STATE_TIMEOUT: Duration = Duration::from_secs(2);

/// Run `job` through `queue` and wait for its result, but no longer than
/// `timeout`. `None` when it could not be queued or did not finish in time; a
/// job that runs late still runs, with nobody waiting for it any more.
fn run_bounded<T: Send + 'static>(
    queue: impl FnOnce(Box<dyn FnOnce() + Send>) -> tauri::Result<()>,
    job: impl FnOnce() -> T + Send + 'static,
    timeout: Duration,
) -> Option<T> {
    // Room for the result, so a job that runs inline (`run_on_main_thread`
    // called on the main thread) does not wait on its own send.
    let (done, result) = std::sync::mpsc::sync_channel(1);
    queue(Box::new(move || {
        let _ = done.send(job());
    }))
    .ok()?;
    result.recv_timeout(timeout).ok()
}

/// The window-state plugin writes its file only when the event loop exits,
/// which an update never gets to. The save runs on the main thread: the
/// plugin keeps its cache locked while it asks each window for its geometry,
/// and asked from any other thread, every one of those questions waits for
/// the main thread — which blocks on that same lock as soon as it handles a
/// move, resize or close of a window first, and the app hangs. On the main
/// thread they are answered inline, as on the plugin's own save at exit.
fn save_window_geometry(app: &AppHandle) {
    let handle = app.clone();
    let saved = run_bounded(
        |task| app.run_on_main_thread(task),
        move || handle.save_window_state(crate::WINDOW_STATE_FLAGS),
        WINDOW_STATE_TIMEOUT,
    );
    match saved {
        Some(Ok(())) => {}
        Some(Err(e)) => tracing::debug!("saving the window state before the update failed: {e}"),
        None => tracing::debug!("the window state was not saved before the update in time"),
    }
}

/// Get the app ready to be ended by the installer. The plugin calls this right
/// before it starts the installer and ends the process with
/// `std::process::exit` right after — no event loop exit, so nothing of what
/// quitting does happens on its own: the last playback report, the downloads,
/// the window geometry and the tray icon are dealt with here.
///
/// Unlike quitting, all of it has to be undoable. The launch can still fail
/// (Smart App Control or an antivirus holding the fresh, unsigned installer,
/// an AppLocker rule on the temp folder), and then the process carries on:
/// the player thread must not be gone, nor the windows torn down
/// (`cleanup_before_exit`), and [`recover_from_failed_launch`] gives the rest
/// back.
///
/// Runs on the install task's blocking thread, not on the main thread, which
/// has work to do meanwhile: the window save runs there, and the player's last
/// state update reaches the tray through it while this waits for the reports.
fn hand_over(app: &AppHandle) {
    // First, so nothing can start playing between the final reports and the
    // end of the process (see `handing_over`).
    HAND_OVER.begin();
    save_window_geometry(app);
    // The same final reports as quitting sends — Jellyfin's "stopped", the
    // ListenBrainz listen, the history row — but the player thread stays.
    if let Some(state) = app.try_state::<AppState>() {
        state.player.stop_and_report(Duration::from_millis(2500));
    }
    if let Some(downloads) = app.try_state::<crate::download::DownloadManager>() {
        downloads.cancel_all();
    }
    crate::tray::set_icon_visible(app, false);
}

/// The installer did not start: give back what [`hand_over`] took away from
/// a process that keeps running. Playback stays stopped but can be started
/// again, and the downloads it cancelled can be retried from their panel.
fn recover_from_failed_launch(app: &AppHandle) {
    if HAND_OVER.end() {
        crate::tray::set_icon_visible(app, true);
    }
}

async fn check_at(app: &AppHandle, source: Source) -> Result<Option<Update>, UpdateError> {
    updater(app, source)?
        .check()
        .await
        .map_err(|e| classify(source.phase(), e))
}

/// What `channel` offers: the stable endpoint's answer, or on beta the newer
/// answer of both endpoints (see the module docs).
async fn check(app: &AppHandle, channel: UpdateChannel) -> Result<Option<Update>, UpdateError> {
    match channel {
        UpdateChannel::Stable => check_at(app, Source::Stable).await,
        UpdateChannel::Beta => {
            let (stable, beta) =
                tokio::join!(check_at(app, Source::Stable), check_at(app, Source::Beta));
            newer_of(stable, beta, |update| {
                semver::Version::parse(&update.version).ok()
            })
        }
    }
}

/// The newer of two answers. One endpoint failing does not fail the check
/// while the other one answered — `classify` logged the failure — and when
/// both fail, the stable endpoint's failure is the one reported.
fn newer_of<T>(
    stable: Result<Option<T>, UpdateError>,
    beta: Result<Option<T>, UpdateError>,
    version: impl Fn(&T) -> Option<semver::Version>,
) -> Result<Option<T>, UpdateError> {
    match (stable, beta) {
        (Ok(Some(stable)), Ok(Some(beta))) => Ok(Some(if version(&beta) > version(&stable) {
            beta
        } else {
            stable
        })),
        (Ok(stable), Ok(beta)) => Ok(stable.or(beta)),
        (Ok(stable), Err(_)) => Ok(stable),
        (Err(_), Ok(beta)) => Ok(beta),
        (Err(stable), Err(_)) => Err(stable),
    }
}

/// Whether this copy was installed rather than unpacked from the portable zip.
///
/// The NSIS installer puts its uninstaller next to the exe, and nothing else
/// does; the portable zip holds the exe alone. Checking for that file beats
/// comparing against install paths, which differ per install mode and
/// language, and it errs on the safe side: an unknown layout counts as
/// portable and merely refuses to install.
fn installed_copy() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("uninstall.exe")))
        .map(|uninstaller| uninstaller.is_file())
        .unwrap_or(false)
}

fn info(app: &AppHandle, update: Option<&Update>) -> UpdateInfo {
    let current_version = app.package_info().version.to_string();
    let installable = installed_copy();
    let Some(update) = update else {
        return UpdateInfo {
            current_version,
            version: None,
            notes: None,
            date: None,
            installable,
        };
    };
    UpdateInfo {
        current_version,
        version: Some(update.version.clone()),
        notes: update.body.clone().filter(|notes| !notes.trim().is_empty()),
        // Straight out of the manifest rather than through `update.date`:
        // re-formatting a parsed timestamp would only invent a format the UI
        // then has to parse back.
        date: update
            .raw_json
            .get("pub_date")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        installable,
    }
}

/// The frontend disables the install button while a track runs, but the rule
/// lives here — a command is reachable whatever the UI shows.
fn refuse_while_playing(app: &AppHandle) -> Result<(), UpdateError> {
    let Some(state) = app.try_state::<AppState>() else {
        return Ok(());
    };
    if matches!(
        state.player.state().status,
        PlaybackStatus::Playing | PlaybackStatus::Loading
    ) {
        return Err(UpdateError::Playing);
    }
    Ok(())
}

/// What the last check that reached the UI said: the background check's
/// announcement or a check from Settings. The announcement is a single event,
/// 20 s after start — with the window hidden or minimized it goes out to a
/// suspended WebView — so the UI reads this back when it comes up or back
/// (`get_known_update`).
///
/// Kept with the channel it was given for: after a switch in Settings, the
/// other channel's answer is no answer at all.
static LAST_ANSWER: std::sync::Mutex<Option<(UpdateChannel, UpdateInfo)>> =
    std::sync::Mutex::new(None);

fn remember(channel: UpdateChannel, info: &UpdateInfo) {
    *LAST_ANSWER.lock().unwrap() = Some((channel, info.clone()));
}

fn known(channel: UpdateChannel) -> Option<UpdateInfo> {
    LAST_ANSWER
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(given, _)| *given == channel)
        .map(|(_, info)| info.clone())
}

/// The last answer, without asking the endpoint again. `None` until a check
/// found something to announce or ran from Settings, and again after the
/// channel was switched.
#[tauri::command]
pub fn get_known_update(app: AppHandle) -> Option<UpdateInfo> {
    known(channel(&app))
}

/// Ask the endpoint what the current version is. Errors are the network's, not
/// the app's: the caller shows them, nothing else changes.
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> AppResult<UpdateInfo> {
    let channel = channel(&app);
    let update = check(&app, channel).await?;
    // Logged like the background check, so the diagnostic export can answer
    // "what did the server actually say" — the panel only ever shows the
    // answer, never which version it was compared against.
    match update.as_ref() {
        Some(update) => tracing::info!(
            version = %update.version,
            current = %update.current_version,
            ?channel,
            "update available"
        ),
        None => tracing::info!(?channel, "checked for updates: nothing newer"),
    }
    let answer = info(&app, update.as_ref());
    remember(channel, &answer);
    Ok(answer)
}

/// Download the update, verify its signature, and hand it to the installer.
///
/// On Windows this does not return: the installer is launched and the process
/// exits right after the hook in [`updater`]. The frontend therefore sees the
/// window close, which is what the confirmation before it says will happen.
/// It returns only when the installer could not be started.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> AppResult<()> {
    if !installed_copy() {
        return Err(UpdateError::Portable.into());
    }
    refuse_while_playing(&app)?;

    let update = check(&app, channel(&app))
        .await?
        .ok_or(UpdateError::NothingNewer)?;

    let mut downloaded: u64 = 0;
    let progress_to = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_to.emit("updater:progress", ProgressEvent { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|e| classify(Phase::Download, e))?;

    // The download runs for a while; playback may have started in it.
    refuse_while_playing(&app)?;

    tracing::info!(version = %update.version, "installing update");
    // Blocking from here on: the hook waits for the player's final reports,
    // and the plugin writes the installer out before starting it.
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await;
    // Still running, so the installer did not start — also when the task
    // itself fell over, which must not leave playback held either.
    recover_from_failed_launch(&app);
    installed
        .map_err(|e| AppError::Other(format!("update install task failed: {e}")))?
        .map_err(|e| classify(Phase::Install, e).into())
}

/// Check once, quietly, a while after startup, and tell the UI only when there
/// is something to install. A failed check is a log line and nothing else —
/// the app does not need the update server to work.
pub fn check_in_background(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        let channel = channel(&app);
        match check(&app, channel).await {
            // Switched in Settings while this asked: the answer is the other
            // channel's, and the UI has already let go of that one.
            Ok(Some(_)) if self::channel(&app) != channel => {
                tracing::debug!("the update channel changed during the background check")
            }
            Ok(Some(update)) => {
                tracing::info!(version = %update.version, ?channel, "update available");
                let answer = info(&app, Some(&update));
                remember(channel, &answer);
                let _ = app.emit("updater:available", answer);
            }
            Ok(None) => tracing::debug!("no update available"),
            // `classify` already logged it; a failed background check is
            // not worth anything louder.
            Err(_) => {}
        }
    });
}

#[cfg(test)]
mod tests {
    //! The updater's configuration decides whether an update can be verified
    //! at all, and a missing field shows up only in a shipped build — where a
    //! wrong answer means either no updates or, worse, unchecked ones.

    use super::{
        beta_endpoint, code_for, describe, http_status, newer_of, run_bounded, stable_offer,
        HandOverFlag, Phase, UpdateError,
    };
    use crate::desktop::UpdateChannel;
    use std::time::{Duration, Instant};
    use tauri_plugin_updater::Error as E;

    fn updater_config() -> serde_json::Value {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        config["plugins"]["updater"].clone()
    }

    #[test]
    fn endpoints_are_https() {
        let endpoints = updater_config()["endpoints"]
            .as_array()
            .expect("updater endpoints")
            .clone();
        assert!(!endpoints.is_empty(), "no update endpoint configured");
        for endpoint in endpoints {
            let url = endpoint.as_str().expect("endpoint string");
            assert!(url.starts_with("https://"), "{url} is not https");
        }
    }

    #[test]
    fn a_public_key_is_configured() {
        let config = updater_config();
        let pubkey = config["pubkey"].as_str().unwrap_or_default();
        assert!(!pubkey.is_empty(), "no updater public key configured");
        // The value is the base64 of a minisign public key file, which always
        // starts with the same comment line -- and base64 encodes a fixed
        // prefix to a fixed prefix, so this catches a key from the wrong tool
        // (or a private key pasted in by mistake) without decoding anything.
        assert!(
            pubkey.starts_with("dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6"),
            "pubkey does not look like a minisign public key"
        );
    }

    #[test]
    fn error_codes_are_stable() {
        // `src/lib/updateErrors.ts` parses exactly these strings.
        use super::UpdateError;
        let wire = |e: UpdateError| crate::error::AppError::from(e).to_string();
        assert_eq!(wire(UpdateError::Playing), "update:playing");
        assert_eq!(wire(UpdateError::Portable), "update:portable");
        assert_eq!(wire(UpdateError::NothingNewer), "update:none");
        assert_eq!(wire(UpdateError::NoRelease), "update:no-release");
        assert_eq!(wire(UpdateError::Offline), "update:offline");
        assert_eq!(wire(UpdateError::Download), "update:download");
        assert_eq!(wire(UpdateError::Signature), "update:signature");
        assert_eq!(wire(UpdateError::Install), "update:install");
        assert_eq!(wire(UpdateError::Unavailable), "update:unavailable");
    }

    /// The release build must not accept an older release. Downgrades are the
    /// one thing a signature cannot rule out: an old installer is validly
    /// signed forever.
    #[test]
    fn downgrades_stay_off() {
        assert!(updater_config()["allowDowngrades"].is_null());
    }

    #[test]
    fn the_last_answer_waits_for_a_ui_that_missed_it() {
        let answer = super::UpdateInfo {
            current_version: "0.6.1".into(),
            version: Some("0.6.2".into()),
            notes: None,
            date: None,
            installable: true,
        };
        super::remember(UpdateChannel::Stable, &answer);
        let known = super::known(UpdateChannel::Stable).expect("remembered");
        assert_eq!(known.version.as_deref(), Some("0.6.2"));
        assert_eq!(known.current_version, "0.6.1");
        // Switched to beta since: that answer was given for stable.
        assert!(super::known(UpdateChannel::Beta).is_none());
    }

    fn v(text: &str) -> semver::Version {
        semver::Version::parse(text).unwrap()
    }

    #[test]
    fn the_stable_endpoint_never_offers_a_pre_release() {
        assert!(stable_offer(&v("0.6.1"), &v("0.6.2")));
        assert!(!stable_offer(&v("0.6.1"), &v("0.7.0-beta.1")));
        assert!(!stable_offer(&v("0.6.2"), &v("0.6.1")));
        // Off the beta, the stable release of that version is the way back.
        assert!(stable_offer(&v("0.7.0-beta.2"), &v("0.7.0")));
        assert!(!stable_offer(&v("0.7.0-beta.2"), &v("0.6.2")));
    }

    #[test]
    fn the_beta_channel_takes_the_newer_answer() {
        let version = |text: &&str| semver::Version::parse(text).ok();
        let pick = |stable, beta| newer_of(stable, beta, version).ok().flatten();
        assert_eq!(
            pick(Ok(Some("0.6.2")), Ok(Some("0.7.0-beta.1"))),
            Some("0.7.0-beta.1")
        );
        // The stable release after the beta wins over a pointer still on it.
        assert_eq!(
            pick(Ok(Some("0.7.0")), Ok(Some("0.7.0-beta.2"))),
            Some("0.7.0")
        );
        assert_eq!(
            pick(Ok(None), Ok(Some("0.7.0-beta.1"))),
            Some("0.7.0-beta.1")
        );
        assert_eq!(pick(Ok(Some("0.6.2")), Ok(None)), Some("0.6.2"));
        assert_eq!(pick(Ok(None), Ok(None)), None);
        // No beta pointer yet, or a failing one: the stable answer stands.
        assert_eq!(
            pick(Ok(Some("0.6.2")), Err(UpdateError::NoRelease)),
            Some("0.6.2")
        );
        assert_eq!(
            pick(Err(UpdateError::Offline), Ok(Some("0.7.0-beta.1"))),
            Some("0.7.0-beta.1")
        );
        let both = newer_of::<&str>(
            Err(UpdateError::Offline),
            Err(UpdateError::NoRelease),
            version,
        );
        assert!(matches!(both, Err(UpdateError::Offline)));
    }

    #[test]
    fn the_beta_endpoint_sits_beside_the_stable_one() {
        let config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let beta = beta_endpoint(&config).expect("plugins.sonarune.betaEndpoint");
        let stable = tauri::Url::parse(updater_config()["endpoints"][0].as_str().unwrap()).unwrap();
        assert_eq!(beta.scheme(), "https");
        assert_eq!(beta.host_str(), stable.host_str());
        // The same repository's releases, at the fixed tag the workflow moves.
        let repo = stable.path().split("/releases/").next().unwrap();
        assert_eq!(
            beta.path(),
            format!("{repo}/releases/download/beta-channel/beta.json")
        );
    }

    /// `latest.json` is not signed, only the installer is. Without this a
    /// manifest could pair a new version number with an older installer whose
    /// signature carries no version. The CLI that signs the releases (2.11.5+)
    /// records the version in the signature's trusted comment.
    #[test]
    fn the_signed_version_is_required() {
        assert_eq!(updater_config()["requireSignedVersion"], true);
    }

    fn wire(e: UpdateError) -> String {
        crate::error::AppError::from(e).to_string()
    }

    fn code(phase: Phase, error: &E) -> String {
        wire(code_for(phase, error))
    }

    /// An error with a URL and a cause, as a failed connection produces it.
    async fn refused_request() -> reqwest::Error {
        reqwest::Client::new()
            .get("http://127.0.0.1:0/releases/latest/download/latest.json")
            .send()
            .await
            .expect_err("nothing listens on port 0")
    }

    #[tokio::test]
    async fn a_dropped_connection_means_offline_only_while_checking() {
        let check = E::Reqwest(refused_request().await);
        assert_eq!(code(Phase::Check, &check), "update:offline");
        // After a successful check, the same failure is the download breaking
        // off — not "the releases page could not be reached".
        let download = E::Reqwest(refused_request().await);
        assert_eq!(code(Phase::Download, &download), "update:download");
    }

    #[test]
    fn a_missing_installer_points_to_the_releases_page() {
        let missing = E::Network("Download request failed with status: 404 Not Found".into());
        assert_eq!(code(Phase::Download, &missing), "update:no-release");
        let forbidden = E::Network("Download request failed with status: 403 Forbidden".into());
        assert_eq!(code(Phase::Download, &forbidden), "update:no-release");
        let server = E::Network("Download request failed with status: 502 Bad Gateway".into());
        assert_eq!(code(Phase::Download, &server), "update:download");
        assert_eq!(
            http_status("Download request failed with status: 404 Not Found"),
            Some(404)
        );
        assert_eq!(http_status("no status here"), None);
    }

    #[test]
    fn signature_failures_are_reported_as_such() {
        let mismatch = E::SignedVersionMismatch {
            signed: "0.6.0".into(),
            announced: "0.6.2".into(),
        };
        assert_eq!(code(Phase::Download, &mismatch), "update:signature");
        assert_eq!(
            code(Phase::Download, &E::MissingSignedVersion),
            "update:signature"
        );
        assert_eq!(
            code(Phase::Download, &E::SignatureUtf8("x".into())),
            "update:signature"
        );
    }

    #[test]
    fn an_unreadable_manifest_is_no_release_not_a_broken_download() {
        let unreadable = E::Serialization(serde_json::from_str::<u32>("{").unwrap_err());
        assert_eq!(code(Phase::Check, &unreadable), "update:no-release");
        assert_eq!(code(Phase::Check, &E::ReleaseNotFound), "update:no-release");
        assert_eq!(
            code(Phase::Check, &E::TargetNotFound("windows-x86_64".into())),
            "update:no-release"
        );
        assert_eq!(code(Phase::Check, &E::EmptyEndpoints), "update:unavailable");
    }

    #[test]
    fn a_failed_launch_is_an_install_failure() {
        let blocked = E::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(code(Phase::Install, &blocked), "update:install");
    }

    #[test]
    fn a_failed_launch_gives_back_only_what_the_hand_over_took() {
        let flag = HandOverFlag::new();
        // Writing the installer out failed: the hook never ran, the tray icon
        // was never taken down.
        assert!(!flag.end());
        flag.begin();
        assert!(flag.is_set());
        assert!(flag.end());
        assert!(!flag.is_set(), "playback has to be possible again");
        assert!(!flag.end());
    }

    #[test]
    fn a_main_thread_that_never_gets_to_the_save_cannot_hold_up_the_hand_over() {
        let parked = std::sync::Mutex::new(Vec::new());
        let started = Instant::now();
        let saved = run_bounded(
            |task| {
                parked.lock().unwrap().push(task);
                Ok(())
            },
            || "saved",
            Duration::from_millis(50),
        );
        assert_eq!(saved, None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_save_on_the_other_thread_reports_back() {
        let saved = run_bounded(
            |task| {
                std::thread::spawn(task);
                Ok(())
            },
            || "saved",
            Duration::from_secs(10),
        );
        assert_eq!(saved, Some("saved"));
    }

    #[test]
    fn a_save_that_runs_inline_does_not_wait_on_itself() {
        // What `run_on_main_thread` does when called on the main thread.
        let saved = run_bounded(
            |task| {
                task();
                Ok(())
            },
            || "saved",
            Duration::ZERO,
        );
        assert_eq!(saved, Some("saved"));
    }

    #[test]
    fn a_save_that_cannot_be_queued_is_not_waited_for() {
        let started = Instant::now();
        let saved = run_bounded(
            |_| Err(tauri::Error::FailedToReceiveMessage),
            || "saved",
            Duration::from_secs(10),
        );
        assert_eq!(saved, None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_failed_request_is_logged_with_its_cause_and_without_its_url() {
        let (detail, error) = describe(E::Reqwest(refused_request().await));
        assert!(matches!(error, E::Reqwest(_)));
        assert!(!detail.contains("127.0.0.1"), "{detail}");
        assert!(!detail.contains("http"), "{detail}");
        // reqwest's own text is just "error sending request"; the cause
        // follows after it.
        assert!(detail.starts_with("error sending request: "), "{detail}");
        assert_ne!(
            crate::diagnostics::redact_log_line(&format!("update check failed: {detail}")),
            "[redacted sensitive log entry]",
            "{detail}"
        );
    }
}
