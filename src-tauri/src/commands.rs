use crate::api::types::{AlbumDto, ArtistDto, LyricsDto, Page, TrackDto, TrackInfoDto};
use crate::api::JellyfinClient;
use crate::cache::{CacheInfo, CoverCache};
use crate::desktop::DesktopSettings;
use crate::error::{AppError, AppResult};
use crate::player::{self, PlayerCommand, PlayerState, QueueTrack};
use crate::store::keys;
use crate::AppState;
use serde::Serialize;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

const KEYRING_SERVICE: &str = "sonarune";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub server_url: String,
    pub username: String,
    pub user_id: String,
    /// Whether this user may permanently delete media on the server. Persisted
    /// at login so the UI can gate the delete action without an extra call.
    pub can_delete: bool,
    /// Whether this user (an admin) may edit item metadata on the server.
    /// Persisted alongside `can_delete` so the UI can gate the edit action.
    pub can_edit: bool,
}

/// Outcome of a `connect` attempt. `CertUntrusted` is returned (not an error)
/// when the server presents a self-signed/untrusted certificate the user has
/// not confirmed yet, so the UI can show the fingerprint and offer to trust it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum ConnectResult {
    Connected { session: SessionInfo },
    CertUntrusted { fingerprint: String },
}

/// `session:ended` — the backend dropped a session that only a new sign-in
/// brings back. The UI returns to the sign-in screen, filled in with the
/// server and the account, and says why; without it the shell stayed signed
/// in while every call failed with "not connected to a server".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnded {
    pub reason: SessionEndReason,
    /// The address to sign in with: the stored one, or where it moved.
    pub server_url: String,
    pub username: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionEndReason {
    /// The server refused the stored credentials (a changed password, a
    /// disabled account, a removed device), or there are none to renew the
    /// rejected token with (a Quick Connect sign-in stores no password).
    Rejected,
    /// The stored address now redirects to `https://` on the same host.
    Moved,
}

/// Sends `session:ended`, and remembers it until the next sign-in so it can be
/// sent again: an event emitted while the window is hidden reaches a suspended
/// WebView and is lost (see `webview_power.rs`), and `check_server` — which
/// runs whenever the window gets the focus back — repeats it then.
pub struct SessionNotice {
    app: tauri::AppHandle,
    last: std::sync::Mutex<Option<SessionEnded>>,
}

impl SessionNotice {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self {
            app,
            last: std::sync::Mutex::new(None),
        }
    }

    fn ended(&self, ended: SessionEnded) {
        *self.last.lock().unwrap() = Some(ended.clone());
        let _ = self.app.emit("session:ended", ended);
    }

    fn repeat(&self) {
        let last = self.last.lock().unwrap().clone();
        if let Some(ended) = last {
            let _ = self.app.emit("session:ended", ended);
        }
    }

    /// A sign-in starts or a session works again: nothing left to repeat.
    fn clear(&self) {
        *self.last.lock().unwrap() = None;
    }
}

/// Drop a session the backend cannot continue, and tell the UI why. Only
/// `stale` is dropped (or nothing, when no session is installed): one that
/// replaced it in the meantime, a sign-in, stays — and then there is nothing
/// to tell either.
async fn end_session(state: &AppState, stale: Option<&Arc<JellyfinClient>>, ended: SessionEnded) {
    {
        let mut session = state.session.write().await;
        let still_stale = match (session.as_ref(), stale) {
            (Some(current), Some(stale)) => Arc::ptr_eq(current, stale),
            (Some(_), None) => false,
            (None, _) => true,
        };
        if !still_stale {
            return;
        }
        *session = None;
    }
    tracing::info!("session ended: {:?}", ended.reason);
    state.session_notice.ended(ended);
}

/// The server address the way it is stored and keyed by: trimmed, without a
/// trailing slash — the form earlier versions saved too, so credentials and
/// pinned certificates stay found. What is not an absolute `http(s)://` URL is
/// refused with a message that says so; reqwest would report only a "builder
/// error" for `localhost:8096`.
fn server_address(raw: &str) -> AppResult<String> {
    let address = raw.trim().trim_end_matches('/').to_string();
    match reqwest::Url::parse(&address) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.has_host() => Ok(address),
        _ => Err(AppError::Other(
            "enter the server address with http:// or https://".into(),
        )),
    }
}

/// Build a client that trusts the fingerprints already pinned for `server_url`.
fn new_client(
    store: &crate::store::Store,
    server_url: &str,
    device_id: &str,
) -> AppResult<JellyfinClient> {
    JellyfinClient::new(
        server_url,
        device_id,
        store.trusted_fingerprints(server_url),
    )
}

/// Like [`new_client`], but also accepting `candidate` for this client only.
/// Nothing is persisted: the certificate is pinned after the login it is used
/// for succeeds (see [`pin_after_login`]).
fn client_trusting(
    store: &crate::store::Store,
    server_url: &str,
    device_id: &str,
    candidate: &str,
) -> AppResult<JellyfinClient> {
    let mut trusted = store.trusted_fingerprints(server_url);
    trusted.push(candidate.to_string());
    JellyfinClient::new(server_url, device_id, trusted)
}

/// The certificate to pin after a successful login made with a client that
/// accepted `confirmed`: the one the server actually presented, and only if it
/// is the confirmed one. `None` when the server turned out to present a
/// publicly valid certificate, or one that was already pinned before — then
/// there is nothing new to trust.
fn pin_after_login(presented: Option<String>, confirmed: &str) -> Option<String> {
    presented.filter(|presented| crate::tls::matches(presented, confirmed))
}

/// The certificate automatic trust may accept: the one a failed handshake
/// presented, when the preference is on and nothing is pinned for the server.
///
/// Trust on **first** use only: once anything is pinned for this server, a
/// different certificate must go through explicit user confirmation
/// (`ConnectResult::CertUntrusted`). Without that check the preference would
/// silently pin whatever an active MITM presents on every later relogin —
/// which is exactly what the pinning in `tls.rs` exists to prevent.
fn autotrust_candidate(
    allow: bool,
    pinned: &[String],
    presented: Option<String>,
) -> Option<String> {
    if !allow || !pinned.is_empty() {
        return None;
    }
    presented
}

/// After an operation failed on an untrusted certificate: when `allow` is set
/// (the "trust this server automatically" preference), a client that also
/// accepts the presented certificate, and its fingerprint, to retry with.
/// Returns `None` when there is nothing to auto-trust.
///
/// Like a confirmed fingerprint in `connect`, the certificate is trusted in
/// memory for this attempt only: whoever uses it pins it with
/// [`trust_after_login`] once the login went through on it. A login that
/// fails, or a Quick Connect that is never approved, leaves nothing trusted.
fn autotrust(
    store: &crate::store::Store,
    server_url: &str,
    device_id: &str,
    client: &JellyfinClient,
    allow: bool,
) -> AppResult<Option<(JellyfinClient, String)>> {
    let pinned = store.trusted_fingerprints(server_url);
    let Some(candidate) = autotrust_candidate(allow, &pinned, client.untrusted_fingerprint())
    else {
        return Ok(None);
    };
    let trusting = client_trusting(store, server_url, device_id, &candidate)?;
    Ok(Some((trusting, candidate)))
}

/// After a login went through on `client`, which also accepted `candidate`:
/// pin the candidate if the server presented it ([`pin_after_login`]), and
/// rebuild the client, session included, with exactly the stored pins — the
/// candidate must not stay accepted in memory when it was not pinned.
fn trust_after_login(
    store: &crate::store::Store,
    server_url: &str,
    device_id: &str,
    client: JellyfinClient,
    candidate: &str,
) -> AppResult<JellyfinClient> {
    if let Some(pin) = pin_after_login(client.presented_fingerprint(), candidate) {
        store.pin_fingerprint(server_url, &pin)?;
    }
    let token = client.token().unwrap_or_default().to_string();
    let user_id = client.user_id()?.to_string();
    Ok(new_client(store, server_url, device_id)?.with_session(token, user_id))
}

fn persist_can_delete(state: &AppState, can_delete: bool) -> AppResult<()> {
    state
        .store
        .set(keys::CAN_DELETE, if can_delete { "true" } else { "false" })
}

fn persist_can_edit(state: &AppState, can_edit: bool) -> AppResult<()> {
    state
        .store
        .set(keys::CAN_EDIT, if can_edit { "true" } else { "false" })
}

/// The keyring service this build uses, set once by [`init_keyring_service`].
static BUILD_KEYRING_SERVICE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Windows keys a credential by service and entry name, not by the app's
/// identifier. A build under another identifier (the e2e suite's
/// `io.sonarune.e2e`, the demo build) would otherwise read, overwrite and
/// delete the installed app's tokens with its own sign-ins and sign-outs, so
/// it gets a service of its own, named after that identifier. The real app
/// keeps the one its credentials already live under.
fn keyring_service_for(identifier: &str) -> &str {
    if identifier == crate::legacy::IDENTIFIER {
        KEYRING_SERVICE
    } else {
        identifier
    }
}

/// Called from `run()` before anything can reach the keyring.
pub(crate) fn init_keyring_service(identifier: &str) {
    let _ = BUILD_KEYRING_SERVICE.set(keyring_service_for(identifier).to_owned());
}

fn keyring_service() -> &'static str {
    BUILD_KEYRING_SERVICE
        .get()
        .map_or(KEYRING_SERVICE, String::as_str)
}

fn keyring_entry(server_url: &str, username: &str) -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(
        keyring_service(),
        &format!("{server_url}::{username}"),
    )?)
}

/// Bring the signed-in account's token and password and the ListenBrainz
/// token over from the keyring service the app used before its rename.
pub(crate) fn migrate_legacy_credentials(store: &crate::store::Store) {
    if let (Some(server_url), Some(username)) =
        (store.get(keys::SERVER_URL), store.get(keys::USERNAME))
    {
        crate::legacy::migrate_credential(KEYRING_SERVICE, &format!("{server_url}::{username}"));
        crate::legacy::migrate_credential(
            KEYRING_SERVICE,
            &format!("{server_url}::{username}::password"),
        );
    }
    crate::legacy::migrate_credential(KEYRING_SERVICE, "listenbrainz");
}

/// The password is kept (Windows Credential Manager) so an expired token can
/// be renewed without bothering the user.
fn password_entry(server_url: &str, username: &str) -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(
        keyring_service(),
        &format!("{server_url}::{username}::password"),
    )?)
}

#[cfg(test)]
mod keyring_tests {
    use super::{keyring_service_for, KEYRING_SERVICE};

    fn identifier(config: &str) -> String {
        let config: serde_json::Value = serde_json::from_str(config).expect("config parses");
        config["identifier"]
            .as_str()
            .expect("identifier")
            .to_owned()
    }

    #[test]
    fn the_installed_app_keeps_its_keyring_service() {
        let app = identifier(include_str!("../tauri.conf.json"));
        assert_eq!(app, crate::legacy::IDENTIFIER);
        assert_eq!(keyring_service_for(&app), "sonarune");
    }

    #[test]
    fn test_and_demo_builds_never_reach_the_installed_apps_credentials() {
        let e2e = identifier(include_str!("../../e2e/tauri.e2e.conf.json"));
        for other in [e2e.as_str(), "io.sonarune.demo"] {
            assert_eq!(keyring_service_for(other), other);
            assert_ne!(keyring_service_for(other), KEYRING_SERVICE);
        }
    }
}

async fn install_session(state: &AppState, client: JellyfinClient) -> Arc<JellyfinClient> {
    // The player worker streams and reports with the same shared session.
    let client = Arc::new(client);
    // A working session clears the "stop trying to re-login" latch.
    state
        .relogin_blocked
        .store(false, std::sync::atomic::Ordering::Relaxed);
    state.session_notice.clear();
    *state.session.write().await = Some(client.clone());
    state.sonic_scan.poke();
    client
}

/// Whether a failed login means the server refused these credentials, as
/// opposed to failing for a while: a 5xx from a restarting server or a 429
/// from Jellyfin's own rate limiter fixes itself in seconds.
fn credentials_rejected(err: &AppError) -> bool {
    matches!(
        err,
        AppError::Server {
            status: 401 | 403,
            ..
        }
    )
}

/// Whether the password itself was refused: a 401. Each one counts toward
/// Jellyfin's lockout. A 403 refuses the account instead (disabled, or no
/// remote access from where the client is), and the password may still be
/// the right one for later.
fn password_refused(err: &AppError) -> bool {
    matches!(err, AppError::Server { status: 401, .. })
}

/// Re-authenticate with the stored credentials and swap the shared session.
/// When the session cannot come back this way — the server refuses the
/// password, or none is stored (a Quick Connect sign-in) — it is ended and
/// the UI asked for a sign-in (`session:ended`).
///
/// Single-flight: `stale` is the client whose call returned 401 at
/// `unauthorized_at`. Callers queue on `relogin_gate`, and whoever finds the
/// session already replaced simply takes over the fresh one instead of logging
/// in again — and whoever finds that the attempt made for their 401 burst
/// failed gets its error instead of trying once more.
async fn relogin(
    state: &AppState,
    stale: Option<Arc<JellyfinClient>>,
    unauthorized_at: Option<std::time::Instant>,
) -> AppResult<Arc<JellyfinClient>> {
    use std::sync::atomic::Ordering;

    let _gate = state.relogin_gate.lock().await;

    let current = { state.session.read().await.clone() };
    match (&stale, current) {
        // Another task renewed while we waited for the gate.
        (Some(stale), Some(current)) if !Arc::ptr_eq(&current, stale) => return Ok(current),
        // No stale client (a window restoring its session) and one is already
        // installed, e.g. by the main window: use it.
        (None, Some(current)) => return Ok(current),
        // The session this 401 belonged to is gone already: signed out while
        // the request was out, or ended by the backend, which said so.
        // Nothing to renew, and nothing to announce again.
        (Some(_), None) => return Err(AppError::NotConnected),
        _ => {}
    }
    if let (Some(stale), Some(at)) = (&stale, unauthorized_at) {
        if let Some(err) = stale.failed_relogin_since(at) {
            return Err(err);
        }
    }
    if state.relogin_blocked.load(Ordering::Relaxed) {
        return Err(AppError::NotConnected);
    }

    let (Some(server_url), Some(username)) = (
        state.store.get(keys::SERVER_URL),
        state.store.get(keys::USERNAME),
    ) else {
        return Err(AppError::NotConnected);
    };
    let Ok(password) = password_entry(&server_url, &username)?.get_password() else {
        // Nothing to renew the rejected token with. Left installed, the dead
        // session would answer every call with a 401, a fresh probe and this
        // same dead end.
        end_session(
            state,
            stale.as_ref(),
            SessionEnded {
                reason: SessionEndReason::Rejected,
                server_url,
                username,
            },
        )
        .await;
        return Err(AppError::NotConnected);
    };
    let auto_trust = state.store.get(keys::ACCEPT_INVALID_CERTS).as_deref() == Some("true");
    let device_id = state.store.device_id()?;

    let started = std::time::Instant::now();
    let mut client = new_client(&state.store, &server_url, &device_id)?;
    let mut candidate = None;
    let attempt = match client.authenticate(&username, &password).await {
        Err(AppError::Network(err)) => {
            // A self-signed home server whose cert isn't pinned yet: trust it on
            // first use when the server is configured for automatic trust, so
            // installs from before certificate pinning keep working.
            match autotrust(&state.store, &server_url, &device_id, &client, auto_trust)? {
                Some((trusting, fingerprint)) => {
                    client = trusting;
                    candidate = Some(fingerprint);
                    client.authenticate(&username, &password).await
                }
                None => Err(AppError::Network(err)),
            }
        }
        other => other,
    };
    // A sign-in or account switch that went through while this login was on
    // the wire owns the session now: installing this result, or clearing the
    // session over a rejection, would undo it. (Sign-out holds the gate.)
    let current = { state.session.read().await.clone() };
    let unchanged = match (&current, &stale) {
        (Some(current), Some(stale)) => Arc::ptr_eq(current, stale),
        (None, None) => true,
        _ => false,
    };
    if !unchanged {
        if attempt.is_ok() {
            let _ = client.logout().await;
        }
        return current.ok_or(AppError::NotConnected);
    }
    let user = match attempt {
        Ok(user) => user,
        Err(err) => {
            // Latch only on an actual rejection of the credentials. Latching
            // on a transient failure would kill the session until the next
            // manual sign-in for something that fixes itself in seconds.
            if credentials_rejected(&err) {
                tracing::warn!("automatic re-login rejected, giving up until next sign-in");
                state.relogin_blocked.store(true, Ordering::Relaxed);
                // The latch lasts for this process only. Kept, a refused
                // password would be tried again at every start, each time one
                // more failed login on the server's count, which locks the
                // account at its limit. The dead token stays: with it, the next
                // start learns that a sign-in is needed without a login.
                if password_refused(&err) {
                    if let Ok(entry) = password_entry(&server_url, &username) {
                        let _ = entry.delete_credential();
                    }
                }
                end_session(
                    state,
                    stale.as_ref(),
                    SessionEnded {
                        reason: SessionEndReason::Rejected,
                        server_url,
                        username,
                    },
                )
                .await;
            } else if let Some(stale) = &stale {
                stale.record_failed_relogin(started, &err);
            }
            return Err(err);
        }
    };
    tracing::info!("session renewed");

    if let Some(candidate) = candidate.as_deref() {
        client = trust_after_login(&state.store, &server_url, &device_id, client, candidate)?;
    }
    keyring_entry(&server_url, &username)?.set_password(client.token().unwrap_or_default())?;
    state.store.set(keys::USER_ID, &user.id)?;
    state.store.settle_auto_trust(&server_url, auto_trust)?;
    persist_can_delete(state, user.can_delete())?;
    persist_can_edit(state, user.can_edit_metadata())?;
    Ok(install_session(state, client).await)
}

/// Whether a 401 calls for a re-login, given what the token probe found.
/// Only a token the server rejects does: a valid one means the 401 was a
/// permission failure, and an unreachable server says nothing either way.
fn relogin_after_401(validity: &crate::api::SessionValidity) -> bool {
    matches!(validity, crate::api::SessionValidity::Invalid)
}

/// Run a client call; on a 401 from an expired/revoked token renew the session
/// once and retry.
///
/// Jellyfin also answers some permission failures with 401 (`DELETE /Items`
/// for an item the user may not delete, for one). A re-login does not fix
/// those, and it rotates the device token server-side, which can break the
/// later range requests of a stream already playing. So a 401 is checked
/// against a cheap authenticated probe (`/Users/Me`) first: a still-valid token
/// returns the original error unchanged, a rejected one (401/403) renews as
/// before, and a probe that cannot reach the server returns the original error
/// too (the next call checks again).
///
/// Bursts: when a token expires, every call in flight gets its 401 at once, and
/// a probe plus a login per call is the kind of auth traffic that trips rate
/// limiting (Jellyfin's or a proxy's). It stays at one of each:
/// - A call that comes back after the session was already replaced skips the
///   probe and retries on the new session.
/// - `validate_after` is single-flight per client: calls whose 401 arrived
///   before the running probe started wait for it and share its answer.
/// - `relogin` is single-flight on `relogin_gate` and hands everyone after the
///   first the renewed session instead of logging in again — or, when that
///   login failed, its error.
///
/// The probe deliberately does not take `relogin_gate`. Sign-out holds that gate
/// too, and permission 401s (which never log in) should not queue behind a login
/// or a sign-out.
pub(crate) async fn with_retry<T, F, Fut>(state: &AppState, call: F) -> AppResult<T>
where
    F: Fn(Arc<JellyfinClient>) -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    let client = client(state).await?;
    let unauthorized = match call(client.clone()).await {
        Err(err @ AppError::Server { status: 401, .. }) => err,
        other => return other,
    };
    let unauthorized_at = std::time::Instant::now();

    // Renewed (or switched) while this call was on the wire: the 401 belongs to
    // the old token, so retry on the current session without probing.
    let current = { state.session.read().await.clone() };
    if let Some(current) = current.filter(|current| !Arc::ptr_eq(current, &client)) {
        return call(current).await;
    }

    let validity = client.validate_after(unauthorized_at).await;
    if !relogin_after_401(&validity) {
        // No "token" in the message: the diagnostics ring drops such lines.
        tracing::info!("got 401 but the session check found {validity:?}, not re-logging in");
        return Err(unauthorized);
    }
    tracing::info!("got 401 and the session was rejected, attempting automatic re-login");
    let client = relogin(state, Some(client), Some(unauthorized_at)).await?;
    call(client).await
}

#[tauri::command]
pub async fn restore_session(state: State<'_, AppState>) -> AppResult<Option<SessionInfo>> {
    restore_stored_session(&state).await
}

/// Bring back the session stored on this device. Runs at startup from Rust
/// (lib.rs), so the tray, the media keys, the library watch and the sonic
/// scan have a session while the window is still hidden — with "start
/// minimized" its WebView may be suspended before the page ever asks — and
/// again for every window that restores it.
///
/// Single-flight on `restore_gate`, and a session already installed for the
/// stored account is handed back as it is. Building a new one would swap the
/// client that every window and background task shares (the projector
/// restores on every open), dropping its scan caches and restarting the
/// library analysis.
pub(crate) async fn restore_stored_session(state: &AppState) -> AppResult<Option<SessionInfo>> {
    let _restoring = state.restore_gate.lock().await;
    let (Some(server_url), Some(username), Some(user_id)) = (
        state.store.get(keys::SERVER_URL),
        state.store.get(keys::USERNAME),
        state.store.get(keys::USER_ID),
    ) else {
        return Ok(None);
    };
    let installed = { state.session.read().await.clone() };
    if installed.is_some_and(|client| {
        client.base_url() == server_url && client.user_id().ok() == Some(user_id.as_str())
    }) {
        return Ok(Some(stored_session_info(
            state, server_url, username, user_id,
        )));
    }
    let auto_trust = state.store.get(keys::ACCEPT_INVALID_CERTS).as_deref() == Some("true");

    let Ok(token) = keyring_entry(&server_url, &username)?.get_password() else {
        return Ok(None);
    };

    let device_id = state.store.device_id()?;
    let mut client = new_client(&state.store, &server_url, &device_id)?
        .with_session(token.clone(), user_id.clone());

    let mut validity = client.validate().await;
    // Trust-on-first-use migration: a self-signed server that predates pinning
    // reads as Untrusted (its cert is rejected). Trust it and re-validate when
    // automatic trust is enabled, rather than logging the user out.
    if matches!(validity, crate::api::SessionValidity::Untrusted(_)) {
        if let Some((trusting, candidate)) =
            autotrust(&state.store, &server_url, &device_id, &client, auto_trust)?
        {
            let trusting = trusting.with_session(token.clone(), user_id.clone());
            // Pinned only when the stored account itself answered over this
            // certificate — this restore's login —, the test a certificate
            // trusted inside a session passes: any 2xx would also take a
            // portal that answers everything.
            match check_confirmed_certificate(&trusting, &candidate).await {
                Ok(CertificateCheck::Pin(_)) => {
                    validity = crate::api::SessionValidity::Valid;
                    client = trust_after_login(
                        &state.store,
                        &server_url,
                        &device_id,
                        trusting,
                        &candidate,
                    )?;
                }
                outcome => {
                    if matches!(outcome, Ok(CertificateCheck::Rejected)) {
                        validity = crate::api::SessionValidity::Invalid;
                    }
                    // Not pinned: whatever follows uses exactly the stored pins.
                    client = new_client(&state.store, &server_url, &device_id)?
                        .with_session(token.clone(), user_id.clone());
                }
            }
        }
    }

    let refresh_policy = match validity {
        crate::api::SessionValidity::Valid => {
            // Also retires the preference on installs that carry it for a
            // server with a publicly valid certificate (see settle_auto_trust).
            state.store.settle_auto_trust(&server_url, auto_trust)?;
            install_session(state, client).await;
            true
        }
        crate::api::SessionValidity::Invalid => {
            // Token expired or revoked — try the stored password before
            // giving up.
            tracing::info!("stored session is no longer valid, trying re-login");
            if relogin(state, None, None).await.is_err() {
                return Ok(None);
            }
            // relogin already persisted the policy returned by authenticate.
            false
        }
        crate::api::SessionValidity::Moved(address) => {
            // The server forces https now. The stored address never works
            // again, so no offline grace: back to the sign-in screen, with
            // the new address filled in.
            end_session(
                state,
                None,
                SessionEnded {
                    reason: SessionEndReason::Moved,
                    server_url: address,
                    username,
                },
            )
            .await;
            return Ok(None);
        }
        crate::api::SessionValidity::Unreachable | crate::api::SessionValidity::Untrusted(_) => {
            // Offline grace: keep the session so the restored queue and UI
            // work; requests recover (or re-login via 401 retry) once the
            // server is back. A certificate that is not trusted keeps it too:
            // a captive portal presents one and goes away again, and for a
            // renewed one the shell offers the fingerprint (`check_server`).
            tracing::info!("server unreachable during restore, keeping session");
            install_session(state, client).await;
            // Do not immediately repeat the failed network round-trip below.
            false
        }
    };

    // Refresh the delete permission from the live policy when reachable — a
    // session restored from before this feature (or a changed server-side
    // permission) would otherwise carry a stale/absent value.
    if refresh_policy {
        // Bind outside the `if let`: on edition 2021 the read guard produced by
        // the scrutinee lives to the end of the block, so it would be held
        // across the `me()` round-trip and stall the player thread.
        let session = { state.session.read().await.clone() };
        if let Some(client) = session {
            if let Ok(user) = client.me().await {
                let _ = persist_can_delete(state, user.can_delete());
                let _ = persist_can_edit(state, user.can_edit_metadata());
            }
        }
    }
    Ok(Some(stored_session_info(
        state, server_url, username, user_id,
    )))
}

fn stored_session_info(
    state: &AppState,
    server_url: String,
    username: String,
    user_id: String,
) -> SessionInfo {
    SessionInfo {
        server_url,
        username,
        user_id,
        can_delete: state.store.get(keys::CAN_DELETE).as_deref() == Some("true"),
        can_edit: state.store.get(keys::CAN_EDIT).as_deref() == Some("true"),
    }
}

#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    downloads: State<'_, crate::download::DownloadManager>,
    server_url: String,
    username: String,
    password: String,
    // "Trust this server's certificate automatically" — skip the confirmation
    // prompt on first use and pin the certificate the server presents.
    accept_invalid_certs: bool,
    // A fingerprint the user just confirmed in the untrusted-certificate prompt.
    // Accepted for this attempt only and pinned once the login succeeds — a
    // failed login (wrong password, wrong server) must not leave it trusted.
    trust_fingerprint: Option<String>,
) -> AppResult<ConnectResult> {
    let server_url = server_address(&server_url)?;
    let device_id = state.store.device_id()?;
    // The ended session this sign-in replaces: `check_server` must not tell
    // the UI about it again while the new one is being made.
    state.session_notice.clear();

    if let Some(fingerprint) = trust_fingerprint.as_deref() {
        if !crate::tls::is_valid_fingerprint(fingerprint) {
            return Err(AppError::Other(
                "not a SHA-256 certificate fingerprint".into(),
            ));
        }
    }
    let mut confirmed = trust_fingerprint;
    let mut client = match confirmed.as_deref() {
        Some(fingerprint) => client_trusting(&state.store, &server_url, &device_id, fingerprint)?,
        None => new_client(&state.store, &server_url, &device_id)?,
    };
    let user = match client.authenticate(&username, &password).await {
        Ok(user) => user,
        Err(AppError::Network(err)) => {
            let Some(fingerprint) = client.untrusted_fingerprint() else {
                return Err(AppError::Network(err));
            };
            // Untrusted certificate. Ask the user to confirm the fingerprint
            // unless they opted into automatic trust AND this is the first use:
            // nothing pinned and nothing confirmed for this attempt. A server
            // that already has a pin, or that presents something other than
            // what was just confirmed, is a certificate *change* — that always
            // needs confirmation.
            if !accept_invalid_certs
                || confirmed.is_some()
                || !state.store.trusted_fingerprints(&server_url).is_empty()
            {
                return Ok(ConnectResult::CertUntrusted { fingerprint });
            }
            client = client_trusting(&state.store, &server_url, &device_id, &fingerprint)?;
            let user = client.authenticate(&username, &password).await?;
            confirmed = Some(fingerprint);
            user
        }
        Err(other) => return Err(other),
    };

    if let Some(confirmed) = confirmed.as_deref() {
        // Only now, with the login through on this certificate, does it become
        // trusted — replacing any earlier pin for the server.
        if let Some(pin) = pin_after_login(client.presented_fingerprint(), confirmed) {
            state.store.pin_fingerprint(&server_url, &pin)?;
        }
        // The client that just logged in also accepted whatever was pinned
        // before, and the candidate whether it got pinned or not. The session
        // must trust exactly the stored pins: a certificate replaced a moment
        // ago may not stay accepted until the next start — and the player
        // copies this client's list for its stream client.
        let token = client.token().unwrap_or_default().to_string();
        client =
            new_client(&state.store, &server_url, &device_id)?.with_session(token, user.id.clone());
    }

    forget_previous_login(&state, &downloads, &server_url, &username, &user.id);
    keyring_entry(&server_url, &username)?.set_password(client.token().unwrap_or_default())?;
    password_entry(&server_url, &username)?.set_password(&password)?;
    state.store.set(keys::SERVER_URL, &server_url)?;
    state.store.set(keys::USERNAME, &username)?;
    state.store.set(keys::USER_ID, &user.id)?;
    state
        .store
        .settle_auto_trust(&server_url, accept_invalid_certs)?;
    let can_delete = user.can_delete();
    let can_edit = user.can_edit_metadata();
    persist_can_delete(&state, can_delete)?;
    persist_can_edit(&state, can_edit)?;

    let info = SessionInfo {
        server_url,
        username,
        user_id: user.id,
        can_delete,
        can_edit,
    };
    install_session(&state, client).await;
    Ok(ConnectResult::Connected { session: info })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickConnectSession {
    pub secret: String,
    pub code: String,
}

/// A certificate a Quick Connect handshake trusts on first use, from
/// `quick_connect_start` to the sign-in that ends it. Only the one the start
/// was shown is accepted by the polls, and it is pinned when the sign-in went
/// through on it — never before. One handshake at a time, as on the setup
/// screen: a new start replaces it.
struct QuickConnectTrust {
    server_url: String,
    secret: String,
    fingerprint: String,
}

impl QuickConnectTrust {
    /// The certificate to accept for a poll of this very handshake.
    fn for_poll(&self, server_url: &str, secret: &str, allow: bool) -> Option<String> {
        (allow && self.server_url == server_url && self.secret == secret)
            .then(|| self.fingerprint.clone())
    }
}

static QUICK_CONNECT_TRUST: std::sync::Mutex<Option<QuickConnectTrust>> =
    std::sync::Mutex::new(None);

/// Start a Quick Connect handshake: returns the code the user confirms on
/// another (already signed-in) Jellyfin device.
#[tauri::command]
pub async fn quick_connect_start(
    state: State<'_, AppState>,
    server_url: String,
    accept_invalid_certs: bool,
) -> AppResult<QuickConnectSession> {
    let server_url = server_address(&server_url)?;
    let device_id = state.store.device_id()?;
    let client = new_client(&state.store, &server_url, &device_id)?;
    *QUICK_CONNECT_TRUST.lock().unwrap() = None;
    let result = match client.quick_connect_initiate().await {
        Err(AppError::Network(err)) => {
            // Quick Connect has no confirmation UI, so it can only reach a
            // self-signed server when automatic trust is enabled.
            match autotrust(
                &state.store,
                &server_url,
                &device_id,
                &client,
                accept_invalid_certs,
            )? {
                Some((trusting, fingerprint)) => {
                    let result = trusting.quick_connect_initiate().await?;
                    *QUICK_CONNECT_TRUST.lock().unwrap() = Some(QuickConnectTrust {
                        server_url,
                        secret: result.secret.clone(),
                        fingerprint,
                    });
                    result
                }
                None => return Err(AppError::Network(err)),
            }
        }
        other => other?,
    };
    Ok(QuickConnectSession {
        secret: result.secret,
        code: result.code,
    })
}

/// Poll the handshake; once approved, signs in and installs the session.
/// Note: Quick Connect yields no password, so automatic re-login on 401 is
/// unavailable for such sessions until the user signs in with a password.
#[tauri::command]
pub async fn quick_connect_poll(
    state: State<'_, AppState>,
    downloads: State<'_, crate::download::DownloadManager>,
    server_url: String,
    accept_invalid_certs: bool,
    secret: String,
) -> AppResult<Option<SessionInfo>> {
    let server_url = server_address(&server_url)?;
    let device_id = state.store.device_id()?;
    // The certificate the start trusted, if it had to: a different one here
    // is a change, and Quick Connect has no prompt to confirm it with.
    let candidate = QUICK_CONNECT_TRUST
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|trust| trust.for_poll(&server_url, &secret, accept_invalid_certs));
    let mut client = match candidate.as_deref() {
        Some(fingerprint) => client_trusting(&state.store, &server_url, &device_id, fingerprint)?,
        None => new_client(&state.store, &server_url, &device_id)?,
    };

    let state_result = client.quick_connect_state(&secret).await?;
    if !state_result.authenticated {
        return Ok(None);
    }
    // As in `connect`: the ended session is being replaced now.
    state.session_notice.clear();
    let user = client.authenticate_with_quick_connect(&secret).await?;
    if let Some(candidate) = candidate.as_deref() {
        // Only now, with the sign-in through on this certificate, does it
        // become trusted.
        client = trust_after_login(&state.store, &server_url, &device_id, client, candidate)?;
        *QUICK_CONNECT_TRUST.lock().unwrap() = None;
    }

    forget_previous_login(&state, &downloads, &server_url, &user.name, &user.id);
    keyring_entry(&server_url, &user.name)?.set_password(client.token().unwrap_or_default())?;
    state.store.set(keys::SERVER_URL, &server_url)?;
    state.store.set(keys::USERNAME, &user.name)?;
    state.store.set(keys::USER_ID, &user.id)?;
    state
        .store
        .settle_auto_trust(&server_url, accept_invalid_certs)?;
    let can_delete = user.can_delete();
    let can_edit = user.can_edit_metadata();
    persist_can_delete(&state, can_delete)?;
    persist_can_edit(&state, can_edit)?;

    let info = SessionInfo {
        server_url,
        username: user.name,
        user_id: user.id,
        can_delete,
        can_edit,
    };
    install_session(&state, client).await;
    Ok(Some(info))
}

#[tauri::command]
pub async fn disconnect(
    state: State<'_, AppState>,
    downloads: State<'_, crate::download::DownloadManager>,
) -> AppResult<()> {
    // No automatic re-login may straddle the sign-out: one finishing after it
    // would store a fresh token and put the session back.
    let _gate = state.relogin_gate.lock().await;
    // The downloads belong to this account too: the queued ones would start
    // under whoever signs in next, and the panel would list this account's
    // tracks to them.
    downloads.reset();
    // Tell the server the playback ended (and flush the ListenBrainz listen)
    // while the token still works — this blocks briefly, so keep it off the
    // async worker threads.
    {
        let player = state.player.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            player.stop_and_report(std::time::Duration::from_millis(2000));
        })
        .await;
    }
    // Then revoke the token server-side. Failure is fine — signing out offline
    // has to work — but when the server is reachable the token should not stay
    // usable.
    {
        let client = { state.session.read().await.clone() };
        if let Some(client) = client {
            if let Err(e) = client.logout().await {
                tracing::debug!("server-side logout failed: {e}");
            }
        }
    }
    if let (Some(server_url), Some(username)) = (
        state.store.get(keys::SERVER_URL),
        state.store.get(keys::USERNAME),
    ) {
        if let Ok(entry) = keyring_entry(&server_url, &username) {
            let _ = entry.delete_credential();
        }
        if let Ok(entry) = password_entry(&server_url, &username) {
            let _ = entry.delete_credential();
        }
    }
    // After the stop report above, which flushed the last listen with it.
    forget_listenbrainz_token(&state);
    // The queue must not outlive the session: its entries hold absolute stream
    // URLs for this server, and leaving them in place would both show the next
    // user what the previous one listened to and aim playback at the old host.
    //
    // Best effort, like the credentials above: the token is revoked by now,
    // and stopping at a failed delete would leave that dead session installed
    // with the UI still signed in. A sign-out always ends the session.
    delete_all(
        &[
            keys::USER_ID,
            keys::CAN_DELETE,
            keys::CAN_EDIT,
            keys::QUEUE,
            keys::QUEUE_INDEX,
        ],
        |key| state.store.delete(key),
    );
    let _ = state.player.send(PlayerCommand::ResetQueue);
    state.session_notice.clear();
    *state.session.write().await = None;
    Ok(())
}

/// Run `delete` for every key, on past a failure (logged).
fn delete_all(keys: &[&str], mut delete: impl FnMut(&str) -> AppResult<()>) {
    for key in keys {
        if let Err(e) = delete(key) {
            tracing::warn!("sign-out could not delete {key}: {e}");
        }
    }
}

/// Every pinned certificate across all servers, for the settings list.
#[tauri::command]
pub fn list_trusted_certificates(state: State<'_, AppState>) -> Vec<crate::store::TrustedCert> {
    state.store.trusted_certs()
}

/// Forget one pinned certificate. The next connection to that server has to
/// confirm its certificate again (or trusts it on first use, when automatic
/// trust is on). A session that is already running keeps the TLS client it was
/// built with — the settings UI signs out when the forgotten pin belongs to
/// the active server, so the change cannot silently wait for the next restart.
#[tauri::command]
pub fn forget_trusted_certificate(
    state: State<'_, AppState>,
    server_url: String,
    fingerprint: String,
) -> AppResult<()> {
    let forgot = state.store.forget_fingerprint(&server_url, &fingerprint)?;
    // Automatic trust is kept only while the signed-in server has a pin
    // (`Store::settle_auto_trust`). With its last one gone, a re-login in the
    // moment before the UI signs out would pin whatever it meets.
    if forgot && state.store.get(keys::SERVER_URL).as_deref() == Some(server_url.as_str()) {
        let requested = state.store.get(keys::ACCEPT_INVALID_CERTS).as_deref() == Some("true");
        state.store.settle_auto_trust(&server_url, requested)?;
    }
    Ok(())
}

/// What the request through a certificate confirmed for the running session
/// decides (see [`trust_session_certificate`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CertificateCheck {
    /// The session's own account answered through it: pin this — `None` when
    /// the server presented a certificate that needs no pin after all.
    Pin(Option<String>),
    /// The server refused the session's token through it.
    Rejected,
}

/// Ask the server behind a confirmed certificate who this session is, through
/// `trusting` — a client that accepts `confirmed` on top of the stored pins
/// and carries the session's token — and decide what to pin.
///
/// Only an answer that is this session's own account stands in for the login
/// a pin needs: a captive portal or an interception answers `200` with a page
/// of its own just as well, and pinning on that would replace the real
/// server's pin with the portal's. A confirmed sign-in clears the same bar —
/// its answer has to be a real Jellyfin login.
pub(crate) async fn check_confirmed_certificate(
    trusting: &JellyfinClient,
    confirmed: &str,
) -> AppResult<CertificateCheck> {
    let user_id = trusting.user_id()?.to_string();
    match trusting.me().await {
        Ok(user) if user.id == user_id => Ok(CertificateCheck::Pin(pin_after_login(
            trusting.presented_fingerprint(),
            confirmed,
        ))),
        Ok(_) => Err(AppError::Other(
            "the server answered for another account".into(),
        )),
        Err(err) if credentials_rejected(&err) => Ok(CertificateCheck::Rejected),
        Err(AppError::Network(_)) if trusting.untrusted_fingerprint().is_some() => Err(
            AppError::Other("the server presents a different certificate now".into()),
        ),
        Err(AppError::Network(_)) => Err(AppError::Other("the server cannot be reached".into())),
        Err(err) => Err(err),
    }
}

/// Trust the certificate the running session's server presents now, after the
/// user compared its fingerprint — the shell offers it when `check_server`
/// reports a certificate that is not trusted (a pinned one renewed). No new
/// sign-in, and the queue stays: the same rules as a confirmed sign-in, with
/// the session's own token as the login. The certificate is accepted in memory
/// for one authenticated request, pinned only when the session's own account
/// answered it on exactly this certificate ([`check_confirmed_certificate`]) —
/// replacing the server's earlier pin — and the session is then rebuilt with
/// exactly the stored pins.
#[tauri::command]
pub async fn trust_session_certificate(
    state: State<'_, AppState>,
    fingerprint: String,
) -> AppResult<()> {
    if !crate::tls::is_valid_fingerprint(&fingerprint) {
        return Err(AppError::Other(
            "not a SHA-256 certificate fingerprint".into(),
        ));
    }
    // No re-login or sign-out may swap the session underneath.
    let _gate = state.relogin_gate.lock().await;
    let current = client(&state).await?;
    // Only what this session was shown: a certificate that changed again
    // since the user looked has to be looked at again.
    if !current
        .untrusted_fingerprint()
        .is_some_and(|presented| crate::tls::matches(&presented, &fingerprint))
    {
        return Err(AppError::Other(
            "the server presents a different certificate now".into(),
        ));
    }
    let (Some(token), Ok(user_id)) = (current.token(), current.user_id()) else {
        return Err(AppError::NotConnected);
    };
    let (token, user_id) = (token.to_string(), user_id.to_string());
    let server_url = current.base_url().to_string();
    let device_id = state.store.device_id()?;

    let trusting = client_trusting(&state.store, &server_url, &device_id, &fingerprint)?
        .with_session(token.clone(), user_id.clone());
    let pin = match check_confirmed_certificate(&trusting, &fingerprint).await? {
        CertificateCheck::Pin(pin) => pin,
        CertificateCheck::Rejected => {
            // The token is gone as well, and a re-login cannot get past the
            // old pin: a full sign-in, which asks for this certificate again.
            end_session(
                &state,
                Some(&current),
                SessionEnded {
                    reason: SessionEndReason::Rejected,
                    server_url,
                    username: state.store.get(keys::USERNAME).unwrap_or_default(),
                },
            )
            .await;
            return Err(AppError::NotConnected);
        }
    };
    // A sign-in that went through meanwhile owns the session (and its pins).
    let unchanged = {
        let session = state.session.read().await;
        session
            .as_ref()
            .is_some_and(|session| Arc::ptr_eq(session, &current))
    };
    if !unchanged {
        return Ok(());
    }
    if let Some(pin) = pin {
        state.store.pin_fingerprint(&server_url, &pin)?;
    }
    let auto_trust = state.store.get(keys::ACCEPT_INVALID_CERTS).as_deref() == Some("true");
    state.store.settle_auto_trust(&server_url, auto_trust)?;
    let rebuilt = new_client(&state.store, &server_url, &device_id)?.with_session(token, user_id);
    tracing::info!("server certificate confirmed for the running session");
    install_session(&state, rebuilt).await;
    Ok(())
}

/// Whether a sign-in to `new` (server URL, user id) continues the account the
/// stored `old` pair belongs to — the pair `load_persisted_queue` checks on
/// startup. By user id, not by name: Jellyfin matches user names regardless of
/// case, so "alice" and "Alice" are one account.
fn same_account(old: (Option<&str>, Option<&str>), new: (&str, &str)) -> bool {
    old == (Some(new.0), Some(new.1))
}

/// Clean up after the login that is being replaced. Call before storing the
/// new `SERVER_URL`/`USERNAME`/`USER_ID`.
///
/// Two things would otherwise be left behind: the previous account's token and
/// plaintext password stay in the Credential Manager forever (their key is
/// derived from server+username, so `disconnect` can no longer address them),
/// and the previous server's queue stays in SQLite with absolute stream URLs
/// pointing at that host. The ListenBrainz token saved under that account goes
/// too, or the new account's listens would land on its profile.
fn forget_previous_login(
    state: &AppState,
    downloads: &crate::download::DownloadManager,
    new_server_url: &str,
    new_username: &str,
    new_user_id: &str,
) {
    let (old_server, old_user, old_user_id) = (
        state.store.get(keys::SERVER_URL),
        state.store.get(keys::USERNAME),
        state.store.get(keys::USER_ID),
    );
    // A different server OR a different account: neither may inherit the
    // queue.
    if !same_account(
        (old_server.as_deref(), old_user_id.as_deref()),
        (new_server_url, new_user_id),
    ) {
        let _ = state.store.delete(keys::QUEUE);
        let _ = state.store.delete(keys::QUEUE_INDEX);
        let _ = state.player.send(PlayerCommand::ResetQueue);
        forget_listenbrainz_token(state);
        // A session the backend ended skipped the sign-out (disconnect):
        // its queued downloads would start under this account.
        downloads.reset();
    }
    let (Some(old_server), Some(old_user)) = (old_server, old_user) else {
        return;
    };
    if (old_server.as_str(), old_user.as_str()) == (new_server_url, new_username) {
        return;
    }
    if let Ok(entry) = keyring_entry(&old_server, &old_user) {
        let _ = entry.delete_credential();
    }
    if let Ok(entry) = password_entry(&old_server, &old_user) {
        let _ = entry.delete_credential();
    }
}

async fn client(state: &AppState) -> AppResult<Arc<JellyfinClient>> {
    state
        .session
        .read()
        .await
        .clone()
        .ok_or(AppError::NotConnected)
}

#[tauri::command]
pub async fn get_albums(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
    genre_id: Option<String>,
    sort: Option<String>,
    favorites_only: Option<bool>,
    played: Option<String>,
) -> AppResult<Page<AlbumDto>> {
    with_retry(&state, |c| {
        let genre_id = genre_id.clone();
        let sort = sort.clone().unwrap_or_default();
        let favorites_only = favorites_only.unwrap_or(false);
        let played = played.clone().unwrap_or_default();
        async move {
            c.albums(
                start_index,
                limit,
                genre_id.as_deref(),
                &sort,
                favorites_only,
                &played,
            )
            .await
        }
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDetail {
    pub album: AlbumDto,
    pub tracks: Vec<TrackDto>,
}

#[tauri::command]
pub async fn get_album(state: State<'_, AppState>, album_id: String) -> AppResult<AlbumDetail> {
    with_retry(&state, |c| {
        let album_id = album_id.clone();
        async move {
            let (album, tracks) =
                crate::api::join_detail(c.album(&album_id), c.album_tracks(&album_id)).await?;
            Ok(AlbumDetail { album, tracks })
        }
    })
    .await
}

#[tauri::command]
pub async fn get_artists(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<ArtistDto>> {
    with_retry(
        &state,
        |c| async move { c.artists(start_index, limit).await },
    )
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistDetail {
    pub artist: ArtistDto,
    pub albums: Vec<AlbumDto>,
    /// Albums the artist appears on but isn't the album artist of.
    pub appears_on: Vec<AlbumDto>,
    pub top_songs: Vec<TrackDto>,
}

#[tauri::command]
pub async fn get_artist(state: State<'_, AppState>, artist_id: String) -> AppResult<ArtistDetail> {
    with_retry(&state, |c| {
        let artist_id = artist_id.clone();
        async move {
            let (artist, albums) =
                crate::api::join_detail(c.artist(&artist_id), c.artist_albums(&artist_id)).await?;
            // Appears-on and top-songs are decoration: a failure must not sink
            // the page.
            let (appears_on, top_songs) = tokio::join!(
                c.artist_appears_on(&artist_id),
                c.artist_top_songs(&artist_id, 5),
            );
            let album_ids: std::collections::HashSet<String> =
                albums.iter().map(|a| a.id.clone()).collect();
            let appears_on = appears_on
                .unwrap_or_default()
                .into_iter()
                .filter(|a| !album_ids.contains(&a.id))
                .collect();
            let top_songs = top_songs.unwrap_or_default();
            Ok(ArtistDetail {
                artist,
                albums,
                appears_on,
                top_songs,
            })
        }
    })
    .await
}

#[tauri::command]
pub async fn get_track_info(
    state: State<'_, AppState>,
    item_id: String,
) -> AppResult<TrackInfoDto> {
    with_retry(&state, |c| {
        let item_id = item_id.clone();
        async move { c.track_info(&item_id).await }
    })
    .await
}

/// Replace characters not allowed in filenames on Windows, and drop the
/// invisible ones that change how the rest of the name is drawn.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|&c| !is_bidi_control(c))
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    // A path component is capped at 255 (UTF-16 units on NTFS, where an emoji
    // counts two). A server-supplied name over that makes every create fail,
    // so the download could never succeed — truncate on a char boundary and
    // keep the extension.
    let cleaned = truncate_filename(&cleaned, MAX_DOWNLOAD_NAME_LEN);
    if cleaned.is_empty() {
        "track".to_string()
    } else if is_windows_reserved_name(&cleaned) {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// Bidirectional formatting characters reorder what follows them on screen:
/// the server-supplied "song\u{202E}calf.exe" shows as "songexe.flac" in
/// Explorer. A file name has no use for them other than disguising what the
/// file is; `char::is_control` does not cover them (they are format, not
/// control characters).
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Longest path component the file systems we run on accept: 255 UTF-16 code
/// units on NTFS, 255 bytes on ext4/APFS.
pub(crate) const MAX_FILENAME_LEN: usize = 255;

/// Longest name `sanitize_filename` produces, in [`filename_len`] units. Leaves
/// room under [`MAX_FILENAME_LEN`] for the `_` reserved-name prefix and the
/// " (n)" collision suffix; the partial-download name shortens its own copy of
/// the name to fit (`download::part_path`).
const MAX_DOWNLOAD_NAME_LEN: usize = 200;

/// Length of a file name in the units the platform caps a path component by:
/// UTF-16 code units on Windows (an emoji counts two), bytes elsewhere.
pub(crate) fn filename_len(name: &str) -> usize {
    if cfg!(windows) {
        name.encode_utf16().count()
    } else {
        name.len()
    }
}

/// The longest prefix of `name` within `max` [`filename_len`] units, never
/// splitting a character.
pub(crate) fn truncate_to_filename_len(name: &str, max: usize) -> &str {
    let mut len = 0;
    for (index, c) in name.char_indices() {
        len += if cfg!(windows) {
            c.len_utf16()
        } else {
            c.len_utf8()
        };
        if len > max {
            return &name[..index];
        }
    }
    name
}

/// Shorten `name` to at most `max` [`filename_len`] units without splitting a
/// character and without losing the extension (which decides how the file
/// opens).
fn truncate_filename(name: &str, max: usize) -> String {
    if filename_len(name) <= max {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        // A "extension" longer than this is not one; treat the whole thing as
        // the stem rather than keeping a huge tail.
        Some((stem, ext)) if !stem.is_empty() && ext.chars().count() <= 10 => (stem, Some(ext)),
        _ => (name, None),
    };
    let keep = max.saturating_sub(ext.map_or(0, |e| filename_len(e) + 1));
    // Windows drops trailing spaces and dots from a name; cut them here so the
    // name that is reserved is the name that gets created.
    let stem = truncate_to_filename_len(stem, keep)
        .trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    match ext {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem.to_string(),
    }
}

/// Device names Windows reserves whatever the extension (`CON.flac` is the
/// console). Windows also ignores spaces before the extension (`CON .flac`),
/// and the COM/LPT digit can be 0-9 or a superscript ¹²³.
fn is_windows_reserved_name(filename: &str) -> bool {
    let base = filename
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    // Compare on chars: a superscript digit is two bytes in UTF-8.
    let mut chars = base.chars();
    let device: String = chars.by_ref().take(3).collect();
    matches!(device.as_str(), "COM" | "LPT")
        && matches!(
            (chars.next(), chars.next()),
            (Some('0'..='9' | '¹' | '²' | '³'), None)
        )
}

pub(crate) fn collision_filename(filename: &str, n: u16) -> String {
    let path = std::path::Path::new(filename);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("track");
    let ext = path.extension().and_then(|s| s.to_str());
    match ext {
        Some(e) => format!("{stem} ({n}).{e}"),
        None => format!("{stem} ({n})"),
    }
}

#[cfg(test)]
mod filename_tests {
    use super::{
        collision_filename, filename_len, is_windows_reserved_name, sanitize_filename,
        truncate_to_filename_len, MAX_FILENAME_LEN,
    };

    #[test]
    fn reserved_device_names_are_recognized_like_windows_does() {
        for reserved in [
            "CON.flac",
            "con .flac",
            "NUL  .mp3",
            "aux",
            "PRN.a.b",
            "CONIN$.txt",
            "COM1.flac",
            "com0.ogg",
            "LPT9",
            "lpt0.flac",
            "COM¹.flac",
            "com².mp3",
            "LPT³.flac",
        ] {
            assert!(is_windows_reserved_name(reserved), "{reserved}");
        }
        for fine in [
            "CONSOLE.flac",
            "Con Air.flac",
            "COM10.flac",
            "COM.flac",
            "COMa.flac",
            "LPT¹0.flac",
            "COM⁴.flac",
            "track.CON",
        ] {
            assert!(!is_windows_reserved_name(fine), "{fine}");
        }
        assert_eq!(sanitize_filename("COM¹.flac"), "_COM¹.flac");
        assert_eq!(sanitize_filename("con .flac"), "_con .flac");
    }

    #[test]
    fn bidi_controls_cannot_disguise_the_extension() {
        // Shown as "songexe.flac" with the override in place.
        assert_eq!(sanitize_filename("song\u{202E}calf.exe"), "songcalf.exe");
        assert_eq!(
            sanitize_filename("\u{2067}a\u{2069}\u{200F}b\u{061C}.flac"),
            "ab.flac"
        );
        // Right-to-left text itself stays.
        assert_eq!(sanitize_filename("שיר.flac"), "שיר.flac");
    }

    #[test]
    fn names_are_bounded_in_file_system_units() {
        // Two UTF-16 units (four bytes) per emoji: 200 chars of these passed
        // the old char cap and still overran NTFS's 255 units.
        let out = sanitize_filename(&format!("{}.flac", "🎵".repeat(300)));
        assert!(filename_len(&out) <= 200, "{} units", filename_len(&out));
        assert!(out.ends_with(".flac"));
        assert!(filename_len(&collision_filename(&out, 999)) <= MAX_FILENAME_LEN);

        // Never cuts inside a character.
        assert_eq!(truncate_to_filename_len("a🎵b", 2), "a");
        assert_eq!(truncate_to_filename_len("a🎵b", filename_len("a🎵")), "a🎵");
        assert_eq!(truncate_to_filename_len("short", 200), "short");

        // A cut must not leave a trailing space or dot, which Windows drops.
        let out = sanitize_filename(&format!("{} .{}", "x".repeat(198), "y".repeat(20)));
        assert!(!out.ends_with([' ', '.']), "{out:?}");
    }
}

/// Atomically create a non-colliding download so concurrent requests can never
/// overwrite an existing file between an existence check and the write.
fn write_unique(
    dir: &std::path::Path,
    filename: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    use std::io::{ErrorKind, Write};

    for n in 0..1000u16 {
        let name = if n == 0 {
            filename.to_string()
        } else {
            collision_filename(filename, n)
        };
        let candidate = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes) {
                    drop(file);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error);
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "too many files with the same download name",
    ))
}

#[tauri::command]
pub fn queue_downloads(
    downloads: State<'_, crate::download::DownloadManager>,
    requests: Vec<crate::download::DownloadRequest>,
) -> Vec<crate::download::DownloadTask> {
    downloads.enqueue(requests)
}

#[tauri::command]
pub fn get_downloads(
    downloads: State<'_, crate::download::DownloadManager>,
) -> Vec<crate::download::DownloadTask> {
    downloads.list()
}

#[tauri::command]
pub fn cancel_download(
    downloads: State<'_, crate::download::DownloadManager>,
    id: String,
) -> AppResult<()> {
    downloads.cancel(&id)
}

#[tauri::command]
pub fn retry_download(
    downloads: State<'_, crate::download::DownloadManager>,
    id: String,
) -> AppResult<()> {
    downloads.retry(&id)
}

#[tauri::command]
pub fn clear_finished_downloads(downloads: State<'_, crate::download::DownloadManager>) {
    downloads.clear_finished();
}

#[tauri::command]
pub fn open_downloads_folder(
    downloads: State<'_, crate::download::DownloadManager>,
) -> AppResult<()> {
    downloads.open_folder()
}

#[tauri::command]
pub async fn get_songs(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<TrackDto>> {
    with_retry(&state, |c| async move { c.songs(start_index, limit).await }).await
}

/// Tracks by id, in the order asked for; ids the server no longer has are left
/// out. Lets a long list re-read the rows it just edited instead of reloading
/// everything from the top.
#[tauri::command]
pub async fn get_tracks(
    state: State<'_, AppState>,
    track_ids: Vec<String>,
) -> AppResult<Vec<TrackDto>> {
    with_retry(&state, |c| {
        let track_ids = track_ids.clone();
        async move { c.tracks_by_ids(&track_ids).await }
    })
    .await
}

pub(crate) fn to_queue_tracks(
    client: &JellyfinClient,
    tracks: Vec<TrackDto>,
) -> AppResult<Vec<QueueTrack>> {
    tracks
        .into_iter()
        .map(|t| {
            Ok(QueueTrack {
                stream_url: client.stream_url(&t.id)?,
                // Identity of the queue line for the UI; the play session the
                // server sees is minted per playback attempt (player/mod.rs).
                entry_id: uuid::Uuid::new_v4().to_string(),
                item_id: t.id,
                name: t.name,
                artist: t.artist,
                album: t.album,
                album_id: t.album_id,
                album_artist: t.album_artist,
                track_number: t.index_number,
                disc_number: t.disc_number,
                duration_ms: t.duration_ms,
                image_item_id: t.image_item_id,
                image_tag: t.image_tag,
                image_blur_hash: t.image_blur_hash,
                normalization_gain: t.normalization_gain,
                album_normalization_gain: t.album_normalization_gain,
                artists: t.artists,
                genres: t.genres,
                source_playlist_id: None,
                auto_dj_reason: None,
                is_favorite: t.is_favorite,
            })
        })
        .collect()
}

#[tauri::command]
pub async fn play_album(
    state: State<'_, AppState>,
    album_id: String,
    start_index: usize,
) -> AppResult<()> {
    let ticket = state.player.play_ticket();
    let queue = with_retry(&state, |c| {
        let album_id = album_id.clone();
        async move {
            let tracks = c.album_tracks(&album_id).await?;
            to_queue_tracks(&c, tracks)
        }
    })
    .await?;
    if queue.is_empty() {
        return Err(AppError::Other("album has no tracks".into()));
    }
    state.player.play_queue(ticket, queue, start_index).await
}

/// Play a track the way clicking it in a track list does: inside its album,
/// from the track on — or on its own, without an album or when the album no
/// longer lists it (retagged, moved). One command, so the ticket is taken
/// before anything is fetched: the newest click wins (`PlayTicket`), which a
/// lookup in the WebView ahead of `play_album` could not promise.
#[tauri::command]
pub async fn play_track_in_album(
    state: State<'_, AppState>,
    track_id: String,
    album_id: Option<String>,
) -> AppResult<()> {
    let ticket = state.player.play_ticket();
    let (queue, start) = with_retry(&state, |c| {
        let track_id = track_id.clone();
        let album_id = album_id.clone().filter(|id| !id.is_empty());
        async move {
            if let Some(album_id) = album_id {
                let tracks = c.album_tracks(&album_id).await?;
                if let Some(start) = tracks.iter().position(|t| t.id == track_id) {
                    return Ok((to_queue_tracks(&c, tracks)?, start));
                }
            }
            let tracks = c.tracks_by_ids(&[track_id]).await?;
            Ok((to_queue_tracks(&c, tracks)?, 0))
        }
    })
    .await?;
    if queue.is_empty() {
        return Err(AppError::Other("the track is not on the server".into()));
    }
    state.player.play_queue(ticket, queue, start).await
}

/// Add a whole album to the queue: `next: true` = right after the current
/// track, otherwise at the end.
#[tauri::command]
pub async fn enqueue_album(
    state: State<'_, AppState>,
    album_id: String,
    next: bool,
) -> AppResult<()> {
    let turn = state.player.enqueue_turn().await;
    let queue = with_retry(&state, |c| {
        let album_id = album_id.clone();
        async move {
            let tracks = c.album_tracks(&album_id).await?;
            to_queue_tracks(&c, tracks)
        }
    })
    .await?;
    state.player.enqueue(turn, queue, next).await
}

/// Add individual tracks (by id) to the queue.
#[tauri::command]
pub async fn enqueue_tracks(
    state: State<'_, AppState>,
    track_ids: Vec<String>,
    next: bool,
) -> AppResult<()> {
    let turn = state.player.enqueue_turn().await;
    let queue = with_retry(&state, |c| {
        let track_ids = track_ids.clone();
        async move {
            let tracks = c.tracks_by_ids(&track_ids).await?;
            to_queue_tracks(&c, tracks)
        }
    })
    .await?;
    state.player.enqueue(turn, queue, next).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub artists: Vec<ArtistDto>,
    pub albums: Vec<AlbumDto>,
    pub tracks: Vec<TrackDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub artists: Vec<ArtistDto>,
    pub albums: Vec<AlbumDto>,
    pub tracks: Vec<TrackDto>,
    pub total: i64,
}

#[tauri::command]
pub async fn search(state: State<'_, AppState>, term: String) -> AppResult<SearchResults> {
    let term = term.trim().to_string();
    if term.is_empty() {
        return Ok(SearchResults {
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
        });
    }
    with_retry(&state, |c| {
        let term = term.clone();
        async move {
            let (artists, albums, tracks) = tokio::try_join!(
                c.search_artists(&term, 12),
                c.search_albums(&term, 18),
                c.search_tracks(&term, 30)
            )?;
            Ok(SearchResults {
                artists,
                albums,
                tracks,
            })
        }
    })
    .await
}

#[tauri::command]
pub async fn search_page(
    state: State<'_, AppState>,
    term: String,
    kind: String,
    start_index: u32,
    limit: u32,
) -> AppResult<SearchPage> {
    let term = term.trim().to_string();
    let kind = kind.to_ascii_lowercase();
    let limit = limit.clamp(1, 200);
    if term.is_empty() {
        return Ok(SearchPage {
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
            total: 0,
        });
    }
    with_retry(&state, |client| {
        let term = term.clone();
        let kind = kind.clone();
        async move {
            let mut result = SearchPage {
                artists: Vec::new(),
                albums: Vec::new(),
                tracks: Vec::new(),
                total: 0,
            };
            match kind.as_str() {
                "artists" => {
                    let page = client
                        .search_artists_page(&term, start_index, limit)
                        .await?;
                    result.artists = page.items;
                    result.total = page.total;
                }
                "albums" => {
                    let page = client.search_albums_page(&term, start_index, limit).await?;
                    result.albums = page.items;
                    result.total = page.total;
                }
                "tracks" => {
                    let page = client.search_tracks_page(&term, start_index, limit).await?;
                    result.tracks = page.items;
                    result.total = page.total;
                }
                _ => return Err(AppError::Other("invalid search category".into())),
            }
            Ok(result)
        }
    })
    .await
}

#[tauri::command]
pub fn player_toggle(state: State<'_, AppState>) -> AppResult<()> {
    state.player.send(PlayerCommand::Toggle)
}

#[tauri::command]
pub fn player_next(state: State<'_, AppState>) -> AppResult<()> {
    state.player.send(PlayerCommand::Next)
}

#[tauri::command]
pub fn player_prev(state: State<'_, AppState>) -> AppResult<()> {
    state.player.send(PlayerCommand::Prev)
}

#[tauri::command]
pub fn player_seek(state: State<'_, AppState>, position_ms: u64) -> AppResult<()> {
    state.player.send(PlayerCommand::Seek { position_ms })
}

#[tauri::command]
pub fn player_set_volume(state: State<'_, AppState>, volume: f32) -> AppResult<()> {
    state.player.send(PlayerCommand::SetVolume { volume })
}

#[tauri::command]
pub fn player_set_shuffle_mode(
    state: State<'_, AppState>,
    shuffle_mode: player::ShuffleMode,
) -> AppResult<()> {
    state
        .player
        .send(PlayerCommand::SetShuffleMode(shuffle_mode))
}

#[tauri::command]
pub fn player_set_repeat(state: State<'_, AppState>, repeat: player::RepeatMode) -> AppResult<()> {
    state.player.send(PlayerCommand::SetRepeat(repeat))
}

#[tauri::command]
pub fn queue_clear(state: State<'_, AppState>) -> AppResult<()> {
    state.player.send(PlayerCommand::ClearQueue)
}

#[tauri::command]
pub fn queue_remove_played(state: State<'_, AppState>) -> AppResult<()> {
    state.player.send(PlayerCommand::RemovePlayed)
}

#[tauri::command]
pub fn queue_remove_duplicates(state: State<'_, AppState>) -> AppResult<()> {
    state.player.send(PlayerCommand::RemoveDuplicates)
}

#[tauri::command]
pub fn queue_undo(state: State<'_, AppState>) -> AppResult<()> {
    state.player.send(PlayerCommand::UndoQueue)
}

#[tauri::command]
pub fn get_player_state(state: State<'_, AppState>) -> PlayerState {
    state.player.state()
}

/// Seek-bar peaks of an already analysed track; `None` until its download
/// has finished. New ones arrive as the `player:waveform` event.
#[tauri::command]
pub fn get_waveform(state: State<'_, AppState>, item_id: String) -> Option<Vec<u8>> {
    state.player.waveform(&item_id)
}

/// Tempo and key of a track analysed earlier (this session or stored). New
/// ones arrive as the `player:analysis` event.
#[tauri::command]
pub fn get_track_analysis(
    state: State<'_, AppState>,
    item_id: String,
) -> Option<player::TrackAnalysisInfo> {
    state.player.track_analysis(&item_id)
}

#[tauri::command]
pub fn get_queue(state: State<'_, AppState>) -> player::QueueSnapshot {
    state.player.queue_snapshot()
}

/// The visualizer opens a binary IPC channel; the player's PCM tap streams the
/// audible signal, folded to stereo, into it while subscribed. `consumer`
/// says what draws from it (a visualizer unless given), which decides whether
/// its window may be suspended in the background (`tap::TapConsumer`).
#[tauri::command]
pub fn visualizer_subscribe(
    state: State<'_, AppState>,
    window: tauri::Window,
    channel: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
    consumer: Option<player::tap::TapConsumer>,
) {
    // Remember which window this belongs to: closing the projector destroys
    // its WebView without the frontend getting to unsubscribe.
    state.player.tap().subscribe(
        window.label().to_string(),
        consumer.unwrap_or_default(),
        channel,
    );
}

#[tauri::command]
pub fn visualizer_unsubscribe(state: State<'_, AppState>, channel_id: u32) {
    state.player.tap().unsubscribe(channel_id);
}

/// Replace the queue with a server-generated mix seeded by any item.
#[tauri::command]
pub async fn play_instant_mix(state: State<'_, AppState>, item_id: String) -> AppResult<()> {
    let ticket = state.player.play_ticket();
    let queue = with_retry(&state, |c| {
        let item_id = item_id.clone();
        async move {
            let tracks = c.instant_mix(&item_id, 100).await?;
            to_queue_tracks(&c, tracks)
        }
    })
    .await?;
    if queue.is_empty() {
        return Err(AppError::Other("the server returned an empty mix".into()));
    }
    state.player.play_queue(ticket, queue, 0).await
}

/// Playback speed, 0.5-2 (clamped); `preserve_pitch` keeps the pitch.
#[tauri::command]
pub fn player_set_speed(
    state: State<'_, AppState>,
    speed: f32,
    preserve_pitch: bool,
) -> AppResult<()> {
    state
        .player
        .send(PlayerCommand::SetSpeed(player::tempo::SpeedSetting {
            speed,
            preserve_pitch,
        }))
}

#[tauri::command]
pub fn set_sleep_timer(
    state: State<'_, AppState>,
    minutes: Option<u32>,
    end_of_track: bool,
    fade_seconds: u32,
) -> AppResult<()> {
    state.player.send(PlayerCommand::SetSleepTimer {
        minutes,
        end_of_track,
        fade_seconds,
    })
}

/// Arm "stop after this track" or "stop after this album" on one queue entry
/// (`entry_id`, which tells two copies of a track apart; `item_id` for a line
/// from before entry ids), or on whatever is playing when both are left out.
/// `StopAfter::Off` clears it.
#[tauri::command]
pub fn player_set_stop_after(
    state: State<'_, AppState>,
    mode: crate::player::StopAfter,
    item_id: Option<String>,
    entry_id: Option<String>,
) -> AppResult<()> {
    let entry = (item_id.is_some() || entry_id.is_some()).then(|| crate::player::EntryRef {
        entry_id: entry_id.unwrap_or_default(),
        item_id: item_id.unwrap_or_default(),
    });
    state
        .player
        .send(PlayerCommand::SetStopAfter { mode, entry })
}

fn listenbrainz_entry() -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(keyring_service(), "listenbrainz")?)
}

/// The stored ListenBrainz token (Windows Credential Manager). Also used by
/// the player worker at startup.
pub(crate) fn listenbrainz_token() -> Option<String> {
    listenbrainz_entry()
        .ok()?
        .get_password()
        .ok()
        .filter(|t| !t.is_empty())
}

/// The ListenBrainz token is a credential of the account it was saved under:
/// on a sign-out, or a sign-in as someone else, it goes from the credential
/// manager and from the player, which keeps its own copy and would scrobble
/// the next account's plays to this profile.
fn forget_listenbrainz_token(state: &AppState) {
    if let Ok(entry) = listenbrainz_entry() {
        let _ = entry.delete_credential();
    }
    let (settings, rewrite) = extras_without_token(state.store.get(keys::EXTRAS).as_deref());
    if let Some(json) = rewrite {
        let _ = state.store.set(keys::EXTRAS, &json);
    }
    let _ = state
        .player
        .send(PlayerCommand::SetExtrasSettings(settings));
}

/// The stored extras without a ListenBrainz token, and the JSON to store when
/// there was one to drop: a build from before the credential manager kept it
/// in the store, and reading the settings would move it over again. Read and
/// written in the stored form, which keeps an Auto-DJ seed of "Sound"
/// ([`player::ExtrasSettings::from_stored_json`]).
fn extras_without_token(stored: Option<&str>) -> (player::ExtrasSettings, Option<String>) {
    let mut settings = stored
        .and_then(player::ExtrasSettings::from_stored_json)
        .unwrap_or_default();
    let rewrite = settings
        .listenbrainz_token
        .take()
        .and_then(|_| settings.to_stored_json().ok());
    (settings, rewrite)
}

/// The token itself never goes back to the WebView — the UI only learns
/// whether one is configured.
#[tauri::command]
pub fn get_extras_settings(state: State<'_, AppState>) -> player::ExtrasSettings {
    let mut settings: player::ExtrasSettings = state
        .store
        .get(crate::store::keys::EXTRAS)
        .and_then(|json| player::ExtrasSettings::from_stored_json(&json))
        .unwrap_or_default();
    // Migrate a token that a pre-keyring build left in the store.
    if let Some(token) = settings.listenbrainz_token.take() {
        if !token.is_empty() {
            if let Ok(entry) = listenbrainz_entry() {
                if entry.set_password(&token).is_ok() {
                    if let Ok(json) = settings.to_stored_json() {
                        let _ = state.store.set(crate::store::keys::EXTRAS, &json);
                    }
                }
            }
        }
    }
    settings.listenbrainz_configured = listenbrainz_token().is_some();
    settings
}

/// Token semantics: `Some(token)` stores it, `Some("")` clears it, `None`
/// keeps the stored one. Only non-secret fields are persisted to SQLite.
#[tauri::command]
pub fn set_extras_settings(
    state: State<'_, AppState>,
    settings: player::ExtrasSettings,
) -> AppResult<()> {
    let mut settings = settings;
    let token = match settings.listenbrainz_token.take() {
        Some(token) if !token.is_empty() => {
            listenbrainz_entry()?.set_password(&token)?;
            Some(token)
        }
        Some(_) => {
            if let Ok(entry) = listenbrainz_entry() {
                let _ = entry.delete_credential();
            }
            None
        }
        None => listenbrainz_token(),
    };
    let json = settings
        .to_stored_json()
        .map_err(|e| AppError::Other(format!("cannot serialize settings: {e}")))?;
    state.store.set(crate::store::keys::EXTRAS, &json)?;
    // The worker gets the effective settings including the real token.
    settings.listenbrainz_token = token;
    state
        .player
        .send(PlayerCommand::SetExtrasSettings(settings))
}

#[tauri::command]
pub fn export_settings(state: State<'_, AppState>) -> AppResult<String> {
    state.store.export_settings()
}

/// Returns the list of imported keys; the player takes them over live.
#[tauri::command]
pub fn import_settings(
    state: State<'_, AppState>,
    cache: State<'_, CoverCache>,
    json: String,
) -> AppResult<Vec<String>> {
    let imported = state.store.import_settings(&json)?;
    let was_imported = |key: &str| imported.iter().any(|k| k == key);
    // Push the freshly imported player-relevant settings live.
    if let Some(raw) = state.store.get(crate::store::keys::PLAYBACK) {
        if let Ok(settings) = serde_json::from_str(&raw) {
            let _ = state.player.send(PlayerCommand::SetPlaybackSettings {
                settings,
                reply: None,
            });
        }
    }
    if let Some(raw) = state.store.get(crate::store::keys::EXTRAS) {
        if let Some(mut settings) = player::ExtrasSettings::from_stored_json(&raw) {
            // Imports never carry a token; keep the locally stored one.
            settings.listenbrainz_token = listenbrainz_token();
            let _ = state
                .player
                .send(PlayerCommand::SetExtrasSettings(settings));
        }
    }
    if let Some(raw) = state.store.get(crate::store::keys::AUDIO_DSP) {
        if let Ok(params) = serde_json::from_str(&raw) {
            state.player.dsp().update(params);
        }
    }
    // The imported sound settles on the device playing now, or with
    // per-device profiles on its older profile would replace it at the next
    // device open.
    if [keys::VOLUME, keys::AUDIO_DSP, keys::AUDIO_PROFILES]
        .iter()
        .any(|key| was_imported(key))
    {
        let volume = was_imported(keys::VOLUME)
            .then(|| state.store.get(keys::VOLUME))
            .flatten()
            .and_then(|raw| raw.parse::<f32>().ok());
        let _ = state.player.send(PlayerCommand::SoundImported {
            volume,
            profiles: was_imported(keys::AUDIO_PROFILES),
        });
    }
    if was_imported(crate::store::keys::PLAYBACK_SPEED) {
        let _ = state
            .player
            .send(PlayerCommand::SetSpeed(player::tempo::SpeedSetting::load(
                &state.store,
            )));
    }
    cache.set_limit_mb(crate::desktop::load(&state.store).cover_cache_limit_mb);
    Ok(imported)
}

#[tauri::command]
pub fn get_playback_settings(state: State<'_, AppState>) -> player::PlaybackSettings {
    state
        .store
        .get(crate::store::keys::PLAYBACK)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// How long a settings change may wait for the player to try an output
/// device (opening a Bluetooth one can take a second or two).
const OUTPUT_SWITCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Returns the settings that apply: an output device the player cannot open
/// is refused and the previous one kept, and Settings has to show that.
#[tauri::command]
pub async fn set_playback_settings(
    state: State<'_, AppState>,
    settings: player::PlaybackSettings,
) -> AppResult<player::PlaybackSettings> {
    let json = serde_json::to_string(&settings)
        .map_err(|e| AppError::Other(format!("cannot serialize settings: {e}")))?;
    state.store.set(crate::store::keys::PLAYBACK, &json)?;
    // Opening a device blocks the player for a moment; wait off the runtime.
    let player = state.player.clone();
    tauri::async_runtime::spawn_blocking(move || {
        player.set_playback_settings(settings, OUTPUT_SWITCH_TIMEOUT)
    })
    .await
    .map_err(|e| AppError::Other(format!("playback settings task failed: {e}")))?
}

#[tauri::command]
pub fn list_audio_devices() -> crate::audio_devices::AudioDeviceSnapshot {
    crate::audio_devices::snapshot()
}

/// Settings' "switch back" after a fallback: play through the configured
/// output device again. Returns the device list as it stands afterwards.
#[tauri::command]
pub async fn restore_output_device(
    state: State<'_, AppState>,
) -> AppResult<crate::audio_devices::AudioDeviceSnapshot> {
    let player = state.player.clone();
    tauri::async_runtime::spawn_blocking(move || {
        player.restore_output(OUTPUT_SWITCH_TIMEOUT)?;
        Ok(crate::audio_devices::snapshot())
    })
    .await
    .map_err(|e| AppError::Other(format!("output switch task failed: {e}")))?
}

#[tauri::command]
pub fn get_desktop_settings(state: State<'_, AppState>) -> DesktopSettings {
    crate::desktop::load(&state.store)
}

#[tauri::command]
pub fn set_desktop_settings(
    state: State<'_, AppState>,
    cache: State<'_, CoverCache>,
    settings: DesktopSettings,
) -> AppResult<DesktopSettings> {
    let settings = crate::desktop::save(&state.store, settings)?;
    cache.set_limit_mb(settings.cover_cache_limit_mb);
    Ok(settings)
}

#[tauri::command]
pub fn set_tray_labels(app: tauri::AppHandle, labels: crate::tray::TrayLabels) {
    crate::tray::set_labels(&app, labels);
}

/// Async with the walk on a blocking thread: sync commands run on the main
/// thread, and a large cover cache is tens of thousands of files.
#[tauri::command]
pub async fn get_cache_info(cache: State<'_, CoverCache>) -> AppResult<CacheInfo> {
    let cache = cache.inner().clone();
    tokio::task::spawn_blocking(move || cache.info())
        .await
        .map_err(|e| AppError::Other(format!("cover cache task failed: {e}")))?
}

/// Async for the same reason as [`get_cache_info`].
#[tauri::command]
pub async fn clear_cover_cache(cache: State<'_, CoverCache>) -> AppResult<CacheInfo> {
    let cache = cache.inner().clone();
    tokio::task::spawn_blocking(move || cache.clear())
        .await
        .map_err(|e| AppError::Other(format!("cover cache task failed: {e}")))?
}

/// Write a deliberately small, redacted diagnostic JSON file to Downloads.
/// The export model never contains server/user ids, URLs, credentials, stream
/// URLs, queue items, track metadata, or filesystem paths.
#[tauri::command]
pub async fn export_diagnostics(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cache: State<'_, CoverCache>,
) -> AppResult<String> {
    let client = { state.session.read().await.clone() };
    let connected = client.is_some();
    let server_version = match client {
        Some(client) => client.server_version().await.ok(),
        None => None,
    };
    let desktop = crate::desktop::load(&state.store);
    let playback = get_playback_settings(state.clone());
    let player_state = state.player.state();
    // A directory walk over the whole cover cache: keep it off the async
    // worker, like `get_cache_info`.
    let cover_cache = {
        let cache = cache.inner().clone();
        tokio::task::spawn_blocking(move || cache.info())
            .await
            .map_err(|e| AppError::Other(format!("cover cache task failed: {e}")))??
    };
    let generated_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let diagnostics = serde_json::json!({
        "schemaVersion": 1,
        "generatedAtUnixMs": generated_at_unix_ms,
        "app": {
            "version": env!("CARGO_PKG_VERSION"),
            "tauriVersion": tauri::VERSION,
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "system": {
            "os": std::env::consts::OS,
            "osVersion": crate::diagnostics::os_version(),
            "architecture": std::env::consts::ARCH,
        },
        "server": {
            "connected": connected,
            "version": server_version,
            "acceptInvalidCertificates": state
                .store
                .get(keys::ACCEPT_INVALID_CERTS)
                .as_deref()
                == Some("true"),
        },
        // Device names stay out: Bluetooth endpoints often carry the owner's
        // name ("AirPods von …").
        "audio": {
            "availableDeviceCount": player::output_devices().len(),
            "customDeviceSelected": playback.output_device.is_some(),
        },
        "playback": {
            "status": player_state.status,
            "queueLength": player_state.queue_len,
            "shuffle": player_state.shuffle,
            "repeat": player_state.repeat,
        },
        "desktop": desktop,
        "coverCache": cover_cache,
        "logs": crate::diagnostics::recent_logs(),
        "redaction": {
            "credentials": "excluded",
            "urls": "excluded",
            "userAndServerIds": "excluded",
            "mediaAndFilesystemPaths": "excluded",
            "queueAndTrackMetadata": "excluded",
        },
    });
    let bytes = serde_json::to_vec_pretty(&diagnostics)
        .map_err(|e| AppError::Other(format!("diagnostic export failed: {e}")))?;
    let dir = app
        .path()
        .download_dir()
        .map_err(|e| AppError::Other(format!("no downloads folder: {e}")))?;
    let destination = tokio::task::spawn_blocking(move || {
        write_unique(&dir, "sonarune-diagnostics.json", &bytes)
    })
    .await
    .map_err(|e| AppError::Other(format!("diagnostic writer task failed: {e}")))?
    .map_err(|e| AppError::Other(format!("diagnostic write failed: {e}")))?;
    Ok(destination.to_string_lossy().into_owned())
}

/// Open the OS file browser at `path`, selecting the file when the platform
/// supports it. Used to show the user exactly where an export landed.
#[tauri::command]
pub async fn reveal_path(app: tauri::AppHandle, path: String) -> AppResult<()> {
    // Only ever reveal something inside the downloads directory. The argument
    // is handed to the file manager, so an arbitrary path — a UNC share in
    // particular — would let anything that can reach this command trigger an
    // outbound SMB connection or open a location the user never chose.
    let downloads = app
        .path()
        .download_dir()
        .map_err(|e| AppError::Other(format!("no downloads folder: {e}")))?;
    // Judge the string before touching the file system: `canonicalize` opens
    // the path, so on `\\attacker\share\x` it would already make that SMB
    // connection (and leak an NTLM handshake) before any check rejected it.
    if !lexically_inside(std::path::Path::new(&path), &downloads) {
        return Err(AppError::Other(
            "path is outside the downloads folder".into(),
        ));
    }
    // What remains still touches the disk (Downloads may be on a network
    // drive), so keep it off the command thread.
    tokio::task::spawn_blocking(move || reveal_in_file_browser(&downloads, &path))
        .await
        .map_err(|e| AppError::Other(format!("reveal task failed: {e}")))?
}

/// Whether `path` is absolute, free of `..`, and under `root`, judged on its
/// components alone — nothing here touches the file system. A Downloads folder
/// on a network drive still works: its path is a prefix like any other.
fn lexically_inside(path: &std::path::Path, root: &std::path::Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && path.starts_with(root)
}

fn reveal_in_file_browser(downloads: &std::path::Path, path: &str) -> AppResult<()> {
    // Canonicalize both sides as well, so symlinks and junctions cannot walk
    // out of the lexically checked location.
    let root = downloads
        .canonicalize()
        .map_err(|e| AppError::Other(format!("no downloads folder: {e}")))?;
    let target = std::path::Path::new(path)
        .canonicalize()
        .map_err(|_| AppError::Other("file not found".into()))?;
    if !target.starts_with(&root) {
        return Err(AppError::Other(
            "path is outside the downloads folder".into(),
        ));
    }
    // Compare on the canonical path, but hand the file manager a normal one:
    // `canonicalize` returns Windows extended-length syntax (\\?\C:\…), and
    // explorer cannot resolve that — it would silently open some default
    // location instead of selecting the file.
    let canonical = target.to_string_lossy().into_owned();
    let path = match canonical.strip_prefix(r"\\?\UNC\") {
        // \\?\UNC\server\share\… is really \\server\share\…
        Some(rest) => format!(r"\\{rest}"),
        None => canonical
            .strip_prefix(r"\\?\")
            .map(str::to_owned)
            .unwrap_or(canonical),
    };

    #[cfg(target_os = "windows")]
    let mut command = reveal_in_explorer(&path);
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(&path);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let target = std::path::Path::new(&path);
        let mut c = std::process::Command::new("xdg-open");
        c.arg(target.parent().unwrap_or(target));
        c
    };
    // explorer.exe exits non-zero even on success, so spawn without waiting.
    command
        .spawn()
        .map_err(|e| AppError::Other(format!("cannot open file browser: {e}")))?;
    Ok(())
}

/// `explorer /select,"<path>"`, written out by hand. Passed as one argument,
/// a path with a space — every export after the first is
/// "sonarune-diagnostics (1).json" — gets the whole `/select,…` quoted, which
/// explorer does not read as the switch, so it opens a default folder instead.
/// A Windows path cannot contain `"`, so quoting it needs no escaping.
#[cfg(target_os = "windows")]
fn reveal_in_explorer(path: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = crate::download::explorer();
    command.raw_arg(format!("/select,\"{path}\""));
    command
}

#[cfg(test)]
mod reveal_tests {
    use super::lexically_inside;
    use std::path::Path;

    #[cfg(windows)]
    #[test]
    fn only_paths_under_downloads_pass_before_the_file_system_is_touched() {
        let root = Path::new(r"C:\Users\u\Downloads");
        let inside = |p: &str| lexically_inside(Path::new(p), root);
        assert!(inside(r"C:\Users\u\Downloads\song.flac"));
        assert!(inside(r"C:\Users\u\Downloads\sub\song.flac"));
        assert!(!inside(r"\\attacker\share\x"));
        assert!(!inside(r"\\?\UNC\attacker\share\x"));
        assert!(!inside(r"\\.\pipe\x"));
        assert!(!inside(r"C:\Users\u\Downloads\..\secret.txt"));
        assert!(!inside(r"C:\Users\u\DownloadsEvil\x"));
        assert!(!inside(r"Downloads\song.flac"));
        assert!(!inside(r"\Users\u\Downloads\song.flac"));

        // Downloads on a network share is still a plain prefix.
        let share = Path::new(r"\\nas\home\Downloads");
        assert!(lexically_inside(
            Path::new(r"\\nas\home\Downloads\song.flac"),
            share
        ));
        assert!(!lexically_inside(
            Path::new(r"\\attacker\home\Downloads\song.flac"),
            share
        ));
    }

    #[cfg(windows)]
    #[test]
    fn explorer_gets_the_select_switch_and_the_quoted_path_as_written() {
        let path = r"C:\Users\Max Mustermann\Downloads\sonarune-diagnostics (1).json";
        let command = super::reveal_in_explorer(path);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [std::ffi::OsStr::new(
                r#"/select,"C:\Users\Max Mustermann\Downloads\sonarune-diagnostics (1).json""#
            )]
        );
        assert!(Path::new(command.get_program()).is_absolute());
    }

    #[cfg(unix)]
    #[test]
    fn only_paths_under_downloads_pass_before_the_file_system_is_touched() {
        let root = Path::new("/home/u/Downloads");
        let inside = |p: &str| lexically_inside(Path::new(p), root);
        assert!(inside("/home/u/Downloads/song.flac"));
        assert!(!inside("/home/u/Downloads/../secret.txt"));
        assert!(!inside("/home/u/DownloadsEvil/x"));
        assert!(!inside("Downloads/song.flac"));
    }
}

#[tauri::command]
pub fn get_audio_settings(state: State<'_, AppState>) -> player::DspParams {
    state.player.dsp().params()
}

/// Applies live (the DSP chain re-reads its parameters mid-stream).
#[tauri::command]
pub fn set_audio_settings(state: State<'_, AppState>, params: player::DspParams) -> AppResult<()> {
    state.player.dsp().update(params.clone());
    let json = serde_json::to_string(&params)
        .map_err(|e| AppError::Other(format!("cannot serialize settings: {e}")))?;
    state.store.set(crate::store::keys::AUDIO_DSP, &json)?;
    match crate::audio_devices::active_device() {
        Some(device) => crate::audio_profiles::remember_dsp(&state.store, &device, &params),
        None => Ok(()),
    }
}

#[tauri::command]
pub fn get_audio_profiles(state: State<'_, AppState>) -> crate::audio_profiles::AudioProfilesInfo {
    crate::audio_profiles::info(&state.store, crate::audio_devices::active_device())
}

#[tauri::command]
pub fn set_audio_profiles_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> AppResult<crate::audio_profiles::AudioProfilesInfo> {
    let volume = state.player.state().volume;
    let active = crate::audio_devices::active_device();
    crate::audio_profiles::set_enabled(
        &state.store,
        enabled,
        active.as_ref(),
        &state.player.dsp().params(),
        volume,
    )?;
    Ok(crate::audio_profiles::info(&state.store, active))
}

/// `device` is the id of the device whose profile goes.
#[tauri::command]
pub fn delete_audio_profile(
    state: State<'_, AppState>,
    device: String,
) -> AppResult<crate::audio_profiles::AudioProfilesInfo> {
    crate::audio_profiles::delete(&state.store, &device)?;
    Ok(crate::audio_profiles::info(
        &state.store,
        crate::audio_devices::active_device(),
    ))
}

/// Ask the library watcher to poll the server right now (fired when the
/// window regains focus, so a change made while away shows up immediately),
/// and return how many changes it has found so far. One found while the
/// window was hidden went out to a suspended WebView, and the watcher's
/// baseline has moved on since: the UI compares the count instead.
#[tauri::command]
pub fn check_library_now(state: State<'_, AppState>) -> u64 {
    state.library_watch.check_now();
    state.library_watch.revision()
}

/// What the shell shows about the server: whether the session reaches it (the
/// offline banner), and the certificate it presents when that is not trusted —
/// a banner of its own, which leads to `trust_session_certificate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCheck {
    pub online: bool,
    pub untrusted_certificate: Option<String>,
}

impl ServerCheck {
    fn online(online: bool) -> Self {
        Self {
            online,
            untrusted_certificate: None,
        }
    }
}

/// The server check behind the offline banner, run on window focus and after
/// a failed action. Online when not signed in — the setup screen handles that
/// case.
#[tauri::command]
pub async fn check_server(state: State<'_, AppState>) -> AppResult<ServerCheck> {
    use crate::api::SessionValidity;

    let client = { state.session.read().await.clone() };
    let Some(client) = client else {
        // A session the backend ended while the window was hidden was never
        // heard of: say it again now that the window is back.
        state.session_notice.repeat();
        return Ok(ServerCheck::online(true));
    };
    Ok(match client.validate().await {
        SessionValidity::Valid | SessionValidity::Invalid => ServerCheck::online(true),
        SessionValidity::Unreachable => ServerCheck::online(false),
        SessionValidity::Untrusted(fingerprint) => ServerCheck {
            online: false,
            untrusted_certificate: Some(fingerprint),
        },
        SessionValidity::Moved(address) => {
            // Forced https while the app runs: the session cannot go on at
            // the stored address (see `restore_session`).
            end_session(
                &state,
                Some(&client),
                SessionEnded {
                    reason: SessionEndReason::Moved,
                    server_url: address,
                    username: state.store.get(keys::USERNAME).unwrap_or_default(),
                },
            )
            .await;
            ServerCheck::online(true)
        }
    })
}

#[tauri::command]
pub async fn get_lyrics(state: State<'_, AppState>, item_id: String) -> AppResult<LyricsDto> {
    with_retry(&state, |c| {
        let item_id = item_id.clone();
        async move { c.lyrics(&item_id).await }
    })
    .await
}

/// Remove the entry at stored index `index` — only while that slot still
/// holds this entry (`entry_id`; `item_id` for a line from before entry ids),
/// so a stale index or a double click cannot take the line that slid into it,
/// not even when that is another copy of the same track.
#[tauri::command]
pub fn queue_remove(
    state: State<'_, AppState>,
    index: usize,
    item_id: String,
    entry_id: Option<String>,
) -> AppResult<()> {
    let entry = crate::player::EntryRef {
        entry_id: entry_id.unwrap_or_default(),
        item_id,
    };
    state.player.send(PlayerCommand::RemoveAt { index, entry })
}

#[tauri::command]
pub fn queue_jump(state: State<'_, AppState>, index: usize) -> AppResult<()> {
    state.player.send(PlayerCommand::JumpTo(index))
}

/// Move a queue entry by play-order position — the order the queue panel
/// shows, i.e. the shuffle order while shuffle is on. `queue_jump` and
/// `queue_remove` take stored queue indices.
#[tauri::command]
pub fn queue_move(state: State<'_, AppState>, from: usize, to: usize) -> AppResult<()> {
    state.player.send(PlayerCommand::MoveTrack { from, to })
}

#[cfg(test)]
mod tests {
    use super::{
        autotrust_candidate, credentials_rejected, delete_all, extras_without_token,
        password_refused, pin_after_login, relogin_after_401, same_account, sanitize_filename,
        server_address, write_unique, QuickConnectTrust, SessionEndReason, SessionEnded,
    };
    use crate::error::AppError;
    use crate::player::{AutoDjSeedMode, ExtrasSettings};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    #[test]
    fn dropping_the_listenbrainz_token_keeps_an_auto_dj_seed_of_sound() {
        // Sign-out and an account change go through here: the extras they
        // hand the player must still say "Sound".
        let stored = ExtrasSettings {
            auto_dj_seed_mode: AutoDjSeedMode::Sonic,
            listenbrainz_token: Some("left over by a pre-keyring build".into()),
            ..ExtrasSettings::default()
        }
        .to_stored_json()
        .unwrap();
        let (settings, rewrite) = extras_without_token(Some(&stored));
        assert_eq!(settings.auto_dj_seed_mode, AutoDjSeedMode::Sonic);
        assert!(settings.listenbrainz_token.is_none());
        let rewritten = ExtrasSettings::from_stored_json(&rewrite.expect("token dropped")).unwrap();
        assert_eq!(rewritten.auto_dj_seed_mode, AutoDjSeedMode::Sonic);
        assert!(rewritten.listenbrainz_token.is_none());

        // Nothing to drop: nothing to write.
        let clean = ExtrasSettings {
            auto_dj_seed_mode: AutoDjSeedMode::Sonic,
            ..ExtrasSettings::default()
        }
        .to_stored_json()
        .unwrap();
        let (settings, rewrite) = extras_without_token(Some(&clean));
        assert_eq!(settings.auto_dj_seed_mode, AutoDjSeedMode::Sonic);
        assert!(rewrite.is_none());
        assert_eq!(
            extras_without_token(None).0.auto_dj_seed_mode,
            AutoDjSeedMode::default()
        );
    }

    #[test]
    fn automatic_trust_takes_only_a_first_certificate() {
        let presented = crate::tls::fingerprint(b"presented");
        let pinned = vec![crate::tls::fingerprint(b"pinned")];
        assert_eq!(
            autotrust_candidate(true, &[], Some(presented.clone())),
            Some(presented.clone())
        );
        // Preference off, or a pin already there: a changed certificate has to
        // be confirmed by the user.
        assert_eq!(
            autotrust_candidate(false, &[], Some(presented.clone())),
            None
        );
        assert_eq!(autotrust_candidate(true, &pinned, Some(presented)), None);
        // The failure was not about a certificate.
        assert_eq!(autotrust_candidate(true, &[], None), None);
    }

    #[test]
    fn quick_connect_trusts_only_the_certificate_its_start_was_shown() {
        let trust = QuickConnectTrust {
            server_url: "https://music.home".into(),
            secret: "secret".into(),
            fingerprint: crate::tls::fingerprint(b"start"),
        };
        assert_eq!(
            trust.for_poll("https://music.home", "secret", true),
            Some(crate::tls::fingerprint(b"start"))
        );
        // Another handshake, another server, or the preference off.
        assert_eq!(trust.for_poll("https://music.home", "other", true), None);
        assert_eq!(trust.for_poll("https://other.home", "secret", true), None);
        assert_eq!(trust.for_poll("https://music.home", "secret", false), None);
    }

    #[test]
    fn a_server_address_needs_its_scheme() {
        assert_eq!(
            server_address(" https://music.example.com/ ").unwrap(),
            "https://music.example.com"
        );
        assert_eq!(
            server_address("http://10.0.0.5:8096").unwrap(),
            "http://10.0.0.5:8096"
        );
        // Kept as typed: credentials and pins are keyed by this exact string.
        assert_eq!(
            server_address("http://NAS:8096/jellyfin/").unwrap(),
            "http://NAS:8096/jellyfin"
        );
        for bad in [
            "localhost:8096",
            "10.0.0.5:8096",
            "nas.local",
            "ftp://nas.local",
            "http://",
            "",
        ] {
            assert!(server_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_a_rejected_token_turns_a_401_into_a_relogin() {
        use crate::api::SessionValidity;
        // Still valid: the 401 was a permission failure, and a re-login would
        // only rotate the device token under a running stream.
        assert!(!relogin_after_401(&SessionValidity::Valid));
        assert!(relogin_after_401(&SessionValidity::Invalid));
        // The probe did not get through: no evidence the token is gone.
        assert!(!relogin_after_401(&SessionValidity::Unreachable));
        assert!(!relogin_after_401(&SessionValidity::Untrusted("AB".into())));
        assert!(!relogin_after_401(&SessionValidity::Moved(
            "https://music.home".into()
        )));
    }

    #[test]
    fn only_a_refused_password_counts_as_rejected_credentials() {
        let server = |status| AppError::Server {
            status,
            message: String::new(),
        };
        assert!(credentials_rejected(&server(401)));
        assert!(credentials_rejected(&server(403)));
        // A restarting server and Jellyfin's rate limiter fix themselves.
        for status in [429, 500, 502, 503] {
            assert!(!credentials_rejected(&server(status)), "{status}");
        }
        assert!(!credentials_rejected(&AppError::NotConnected));
        assert!(!credentials_rejected(&AppError::Other("timeout".into())));

        // Only a refused password is dropped from the credential store; a
        // 403 (account disabled, no remote access from here) may pass later.
        assert!(password_refused(&server(401)));
        assert!(!password_refused(&server(403)));
        assert!(!password_refused(&server(503)));
    }

    #[test]
    fn the_queue_stays_with_the_same_account_whatever_the_name_is_spelled() {
        let stored = (Some("https://music.home"), Some("user-1"));
        // "alice" typed now, "Alice" before: same user id, same account.
        assert!(same_account(stored, ("https://music.home", "user-1")));
        assert!(!same_account(stored, ("https://music.home", "user-2")));
        assert!(!same_account(stored, ("https://other.home", "user-1")));
        // Signed out before (the user id is gone) or never signed in.
        assert!(!same_account(
            (Some("https://music.home"), None),
            ("https://music.home", "user-1")
        ));
        assert!(!same_account(
            (None, None),
            ("https://music.home", "user-1")
        ));
    }

    #[test]
    fn a_sign_out_deletes_every_key_past_a_failure() {
        let mut attempted = Vec::new();
        delete_all(&["a", "b", "c"], |key| {
            attempted.push(key.to_string());
            if key == "a" {
                Err(AppError::Other("disk full".into()))
            } else {
                Ok(())
            }
        });
        assert_eq!(attempted, ["a", "b", "c"]);
    }

    #[test]
    fn session_ended_reaches_the_ui_in_camel_case() {
        let ended = SessionEnded {
            reason: SessionEndReason::Moved,
            server_url: "https://music.home".into(),
            username: "alice".into(),
        };
        assert_eq!(
            serde_json::to_value(&ended).unwrap(),
            serde_json::json!({
                "reason": "moved",
                "serverUrl": "https://music.home",
                "username": "alice",
            })
        );
        assert_eq!(
            serde_json::to_value(SessionEndReason::Rejected).unwrap(),
            "rejected"
        );
    }

    #[test]
    fn only_the_presented_confirmed_certificate_is_pinned_after_login() {
        let confirmed = crate::tls::fingerprint(b"confirmed");
        // The server presented the confirmed certificate (any spelling).
        assert_eq!(
            pin_after_login(Some(confirmed.to_lowercase()), &confirmed),
            Some(confirmed.to_lowercase())
        );
        // Publicly valid certificate: nothing to pin.
        assert_eq!(pin_after_login(None, &confirmed), None);
        // The login went through on a certificate that was pinned before, not
        // the one just confirmed: the confirmation must not become a pin.
        let old = crate::tls::fingerprint(b"old");
        assert_eq!(pin_after_login(Some(old), &confirmed), None);
    }

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        let path =
            std::env::temp_dir().join(format!("sonarune-download-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create test directory");
        TempDir(path)
    }

    #[test]
    fn download_filename_is_safe_on_windows() {
        assert_eq!(sanitize_filename(" bad:/name?.flac "), "bad__name_.flac");
        assert_eq!(sanitize_filename("CON.mp3"), "_CON.mp3");
        assert_eq!(sanitize_filename("lpt9.cover.flac"), "_lpt9.cover.flac");
        assert_eq!(sanitize_filename("..."), "track");
        assert_eq!(sanitize_filename("bad\0name.mp3"), "bad_name.mp3");
    }

    #[test]
    fn concurrent_downloads_never_overwrite_each_other() {
        const WRITERS: usize = 8;
        let dir = temp_dir();
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles: Vec<_> = (0..WRITERS)
            .map(|value| {
                let path = dir.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    write_unique(&path, "song.flac", &[value as u8]).expect("write unique download")
                })
            })
            .collect();

        let paths: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("join writer"))
            .collect();
        assert_eq!(paths.iter().collect::<HashSet<_>>().len(), WRITERS);

        let contents: HashSet<u8> = paths
            .iter()
            .map(|path| std::fs::read(path).expect("read download")[0])
            .collect();
        assert_eq!(contents, (0..WRITERS as u8).collect());
    }

    #[test]
    fn long_server_names_are_truncated_but_keep_their_extension() {
        // Windows caps a path component at 255 chars; an untruncated name
        // made every create fail, so the download could never finish.
        let long = format!("{}.flac", "a".repeat(400));
        let out = sanitize_filename(&long);
        assert!(
            out.chars().count() <= 200,
            "got {} chars",
            out.chars().count()
        );
        assert!(out.ends_with(".flac"), "extension lost: {out}");

        // A dot far from the end is not an extension — don't keep a huge tail.
        let odd = format!("{}.{}", "b".repeat(300), "c".repeat(50));
        let out = sanitize_filename(&odd);
        assert!(out.chars().count() <= 200);

        // Multi-byte names must not be split mid-character.
        let umlauts = format!("{}.mp3", "ä".repeat(400));
        let out = sanitize_filename(&umlauts);
        assert!(out.chars().count() <= 200);
        assert!(out.ends_with(".mp3"));

        // Short names are untouched.
        assert_eq!(sanitize_filename("Track 01.flac"), "Track 01.flac");
    }
}
