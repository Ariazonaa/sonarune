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
//! Update channels (stable/beta) are not a thing yet; there is one endpoint.
//!
//! One thing to know when testing this right after cutting a release: the
//! endpoint is `releases/latest/download/latest.json`, and GitHub serves that
//! redirect from a cache. For the first minutes after publishing it can still
//! point at the release before it, so a check in that window truthfully
//! reports "up to date" — the app asked, and that is what it was told. It
//! sorts itself out on its own; there is nothing to fix here, and a
//! cache-busting parameter on every check would cost everyone a cached
//! response to spare one person a few minutes once per release.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

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

/// Sort a plugin failure into what the user can do about it, and log what
/// actually happened -- the diagnostic export carries the log, the UI does not.
fn classify(context: &str, error: tauri_plugin_updater::Error) -> UpdateError {
    use tauri_plugin_updater::Error as E;
    tracing::warn!("{context}: {error}");
    match error {
        E::ReleaseNotFound
        | E::TargetNotFound(_)
        | E::TargetsNotFound(_)
        | E::UnsupportedArch
        | E::UnsupportedOs => UpdateError::NoRelease,
        E::Reqwest(_) | E::Network(_) => UpdateError::Offline,
        E::Minisign(_) | E::SignatureUtf8(_) | E::Base64(_) => UpdateError::Signature,
        E::EmptyEndpoints => UpdateError::Unavailable,
        _ => UpdateError::Download,
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

/// An updater whose exit hook leaves the app in a clean state: the installer
/// kills this process, so the last playback report and any running download
/// have to be dealt with first — the same order as the quit path.
fn updater(app: &AppHandle) -> Result<Updater, UpdateError> {
    let handle = app.clone();
    app.updater_builder()
        .on_before_exit(move || {
            crate::flush_before_exit(&handle);
            handle.cleanup_before_exit();
        })
        .build()
        .map_err(|e| classify("updater unavailable", e))
}

async fn check(app: &AppHandle) -> Result<Option<Update>, UpdateError> {
    updater(app)?
        .check()
        .await
        .map_err(|e| classify("update check failed", e))
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

/// Ask the endpoint what the current version is. Errors are the network's, not
/// the app's: the caller shows them, nothing else changes.
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> AppResult<UpdateInfo> {
    let update = check(&app).await?;
    // Logged like the background check, so the diagnostic export can answer
    // "what did the server actually say" — the panel only ever shows the
    // answer, never which version it was compared against.
    match update.as_ref() {
        Some(update) => tracing::info!(
            version = %update.version,
            current = %update.current_version,
            "update available"
        ),
        None => tracing::info!("checked for updates: nothing newer"),
    }
    Ok(info(&app, update.as_ref()))
}

/// Download the update, verify its signature, and hand it to the installer.
///
/// On Windows this does not return: the installer is launched and the process
/// exits through the hook in [`updater`]. The frontend therefore sees the
/// window close, which is what the confirmation before it says will happen.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> AppResult<()> {
    if !installed_copy() {
        return Err(UpdateError::Portable.into());
    }
    refuse_while_playing(&app)?;

    let update = check(&app).await?.ok_or(UpdateError::NothingNewer)?;

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
        .map_err(|e| classify("update download failed", e))?;

    // The download runs for a while; playback may have started in it.
    refuse_while_playing(&app)?;

    tracing::info!(version = %update.version, "installing update");
    update.install(bytes).map_err(|e| {
        classify("update install failed", e);
        AppError::from(UpdateError::Install)
    })
}

/// Check once, quietly, a while after startup, and tell the UI only when there
/// is something to install. A failed check is a log line and nothing else —
/// the app does not need the update server to work.
pub fn check_in_background(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        match check(&app).await {
            Ok(Some(update)) => {
                tracing::info!(version = %update.version, "update available");
                let _ = app.emit("updater:available", info(&app, Some(&update)));
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
}
