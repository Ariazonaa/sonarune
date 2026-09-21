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
use tauri::{Manager, State};

const KEYRING_SERVICE: &str = "jellysic";

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

/// After an operation failed on an untrusted certificate: when `allow` is set
/// (the "trust this server automatically" preference), pin the presented
/// fingerprint and hand back a rebuilt, now-trusting client to retry with.
/// Returns `None` when there is nothing to auto-trust.
///
/// Trust on **first** use only: once anything is pinned for this server, a
/// different certificate must go through explicit user confirmation
/// (`ConnectResult::CertUntrusted`). Without that check the preference would
/// silently pin whatever an active MITM presents on every later relogin —
/// which is exactly what the pinning in `tls.rs` exists to prevent.
fn autotrust(
    store: &crate::store::Store,
    server_url: &str,
    device_id: &str,
    client: &JellyfinClient,
    allow: bool,
) -> AppResult<Option<JellyfinClient>> {
    if !allow || !store.trusted_fingerprints(server_url).is_empty() {
        return Ok(None);
    }
    let Some(fingerprint) = client.untrusted_fingerprint() else {
        return Ok(None);
    };
    store.pin_fingerprint(server_url, &fingerprint)?;
    Ok(Some(new_client(store, server_url, device_id)?))
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

fn keyring_entry(server_url: &str, username: &str) -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(
        KEYRING_SERVICE,
        &format!("{server_url}::{username}"),
    )?)
}

/// The password is kept (Windows Credential Manager) so an expired token can
/// be renewed without bothering the user.
fn password_entry(server_url: &str, username: &str) -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(
        KEYRING_SERVICE,
        &format!("{server_url}::{username}::password"),
    )?)
}

async fn install_session(state: &AppState, client: JellyfinClient) -> Arc<JellyfinClient> {
    // The player worker streams and reports with the same shared session.
    let client = Arc::new(client);
    // A working session clears the "stop trying to re-login" latch.
    state
        .relogin_blocked
        .store(false, std::sync::atomic::Ordering::Relaxed);
    *state.session.write().await = Some(client.clone());
    client
}

/// Re-authenticate with the stored credentials and swap the shared session.
/// Fails cleanly when no password is stored (pre-existing installs).
///
/// Single-flight: `stale` is the client whose call returned 401. Callers queue
/// on `relogin_gate`, and whoever finds the session already replaced simply
/// takes over the fresh one instead of logging in again.
async fn relogin(
    state: &AppState,
    stale: Option<Arc<JellyfinClient>>,
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
        _ => {}
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
    let password = password_entry(&server_url, &username)?
        .get_password()
        .map_err(|_| AppError::NotConnected)?;
    let auto_trust = state.store.get(keys::ACCEPT_INVALID_CERTS).as_deref() == Some("true");
    let device_id = state.store.device_id()?;

    let mut client = new_client(&state.store, &server_url, &device_id)?;
    let attempt = match client.authenticate(&username, &password).await {
        Err(AppError::Network(err)) => {
            // A self-signed home server whose cert isn't pinned yet: trust it on
            // first use when the server is configured for automatic trust, so
            // installs from before certificate pinning keep working.
            match autotrust(&state.store, &server_url, &device_id, &client, auto_trust)? {
                Some(rebuilt) => {
                    client = rebuilt;
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
            // Latch only on an actual rejection of the credentials. A 5xx from
            // a restarting server or a 429 from Jellyfin's own rate limiter is
            // transient — latching on those would kill the session until the
            // next manual sign-in for something that fixes itself in seconds.
            if matches!(
                err,
                AppError::Server {
                    status: 401 | 403,
                    ..
                }
            ) {
                tracing::warn!("automatic re-login rejected, giving up until next sign-in");
                state.relogin_blocked.store(true, Ordering::Relaxed);
                *state.session.write().await = None;
            }
            return Err(err);
        }
    };
    tracing::info!("session renewed");

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
fn relogin_after_401(validity: crate::api::SessionValidity) -> bool {
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
///   first the renewed session instead of logging in again.
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
    if !relogin_after_401(validity) {
        // No "token" in the message: the diagnostics ring drops such lines.
        tracing::info!("got 401 but the session check found {validity:?}, not re-logging in");
        return Err(unauthorized);
    }
    tracing::info!("got 401 and the session was rejected, attempting automatic re-login");
    let client = relogin(state, Some(client)).await?;
    call(client).await
}

#[tauri::command]
pub async fn restore_session(state: State<'_, AppState>) -> AppResult<Option<SessionInfo>> {
    let (Some(server_url), Some(username), Some(user_id)) = (
        state.store.get(keys::SERVER_URL),
        state.store.get(keys::USERNAME),
        state.store.get(keys::USER_ID),
    ) else {
        return Ok(None);
    };
    let auto_trust = state.store.get(keys::ACCEPT_INVALID_CERTS).as_deref() == Some("true");

    let Ok(token) = keyring_entry(&server_url, &username)?.get_password() else {
        return Ok(None);
    };

    let device_id = state.store.device_id()?;
    let mut client = new_client(&state.store, &server_url, &device_id)?
        .with_session(token.clone(), user_id.clone());

    let mut validity = client.validate().await;
    // Trust-on-first-use migration: a self-signed server that predates pinning
    // reads as Unreachable (its cert is rejected). Pin it and re-validate when
    // automatic trust is enabled, rather than logging the user out.
    if matches!(validity, crate::api::SessionValidity::Unreachable) {
        if let Some(rebuilt) =
            autotrust(&state.store, &server_url, &device_id, &client, auto_trust)?
        {
            client = rebuilt.with_session(token.clone(), user_id.clone());
            validity = client.validate().await;
        }
    }

    let refresh_policy = match validity {
        crate::api::SessionValidity::Valid => {
            // Also retires the preference on installs that carry it for a
            // server with a publicly valid certificate (see settle_auto_trust).
            state.store.settle_auto_trust(&server_url, auto_trust)?;
            install_session(&state, client).await;
            true
        }
        crate::api::SessionValidity::Invalid => {
            // Token expired or revoked — try the stored password before
            // giving up.
            tracing::info!("stored session is no longer valid, trying re-login");
            if relogin(&state, None).await.is_err() {
                return Ok(None);
            }
            // relogin already persisted the policy returned by authenticate.
            false
        }
        crate::api::SessionValidity::Unreachable => {
            // Offline grace: keep the session so the restored queue and UI
            // work; requests recover (or re-login via 401 retry) once the
            // server is back.
            tracing::info!("server unreachable during restore, keeping session");
            install_session(&state, client).await;
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
                let _ = persist_can_delete(&state, user.can_delete());
                let _ = persist_can_edit(&state, user.can_edit_metadata());
            }
        }
    }
    let can_delete = state.store.get(keys::CAN_DELETE).as_deref() == Some("true");
    let can_edit = state.store.get(keys::CAN_EDIT).as_deref() == Some("true");
    Ok(Some(SessionInfo {
        server_url,
        username,
        user_id,
        can_delete,
        can_edit,
    }))
}

#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
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
    let server_url = server_url.trim().trim_end_matches('/').to_string();
    let device_id = state.store.device_id()?;

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

    forget_previous_login(&state, &server_url, &username);
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

/// Start a Quick Connect handshake: returns the code the user confirms on
/// another (already signed-in) Jellyfin device.
#[tauri::command]
pub async fn quick_connect_start(
    state: State<'_, AppState>,
    server_url: String,
    accept_invalid_certs: bool,
) -> AppResult<QuickConnectSession> {
    let server_url = server_url.trim().trim_end_matches('/').to_string();
    let device_id = state.store.device_id()?;
    let mut client = new_client(&state.store, &server_url, &device_id)?;
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
                Some(rebuilt) => {
                    client = rebuilt;
                    client.quick_connect_initiate().await?
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
    server_url: String,
    accept_invalid_certs: bool,
    secret: String,
) -> AppResult<Option<SessionInfo>> {
    let server_url = server_url.trim().trim_end_matches('/').to_string();
    let device_id = state.store.device_id()?;
    let mut client = new_client(&state.store, &server_url, &device_id)?;

    let state_result = match client.quick_connect_state(&secret).await {
        Err(AppError::Network(err)) => {
            match autotrust(
                &state.store,
                &server_url,
                &device_id,
                &client,
                accept_invalid_certs,
            )? {
                Some(rebuilt) => {
                    client = rebuilt;
                    client.quick_connect_state(&secret).await?
                }
                None => return Err(AppError::Network(err)),
            }
        }
        other => other?,
    };
    if !state_result.authenticated {
        return Ok(None);
    }
    let user = client.authenticate_with_quick_connect(&secret).await?;

    forget_previous_login(&state, &server_url, &user.name);
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
pub async fn disconnect(state: State<'_, AppState>) -> AppResult<()> {
    // No automatic re-login may straddle the sign-out: one finishing after it
    // would store a fresh token and put the session back.
    let _gate = state.relogin_gate.lock().await;
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
    state.store.delete(keys::USER_ID)?;
    state.store.delete(keys::CAN_DELETE)?;
    state.store.delete(keys::CAN_EDIT)?;
    // The queue must not outlive the session: its entries hold absolute stream
    // URLs for this server, and leaving them in place would both show the next
    // user what the previous one listened to and aim playback at the old host.
    state.store.delete(keys::QUEUE)?;
    state.store.delete(keys::QUEUE_INDEX)?;
    let _ = state.player.send(PlayerCommand::ResetQueue);
    *state.session.write().await = None;
    Ok(())
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
    state.store.forget_fingerprint(&server_url, &fingerprint)?;
    Ok(())
}

/// Clean up after the login that is being replaced. Call before storing the
/// new `SERVER_URL`/`USERNAME`.
///
/// Two things would otherwise be left behind: the previous account's token and
/// plaintext password stay in the Credential Manager forever (their key is
/// derived from server+username, so `disconnect` can no longer address them),
/// and the previous server's queue stays in SQLite with absolute stream URLs
/// pointing at that host.
fn forget_previous_login(state: &AppState, new_server_url: &str, new_username: &str) {
    let (old_server, old_user) = (
        state.store.get(keys::SERVER_URL),
        state.store.get(keys::USERNAME),
    );
    // A different server OR a different account: neither may inherit the
    // queue. `load_persisted_queue` checks the same pair on startup.
    if old_server.as_deref() != Some(new_server_url) || old_user.as_deref() != Some(new_username) {
        let _ = state.store.delete(keys::QUEUE);
        let _ = state.store.delete(keys::QUEUE_INDEX);
        let _ = state.player.send(PlayerCommand::ResetQueue);
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
            let (album, tracks) = tokio::try_join!(c.album(&album_id), c.album_tracks(&album_id))?;
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
                tokio::try_join!(c.artist(&artist_id), c.artist_albums(&artist_id))?;
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

/// Replace characters not allowed in filenames on Windows.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
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
    state.player.send(PlayerCommand::PlayQueue {
        tracks: queue,
        start_index,
    })
}

/// Add a whole album to the queue: `next: true` = right after the current
/// track, otherwise at the end.
#[tauri::command]
pub async fn enqueue_album(
    state: State<'_, AppState>,
    album_id: String,
    next: bool,
) -> AppResult<()> {
    let queue = with_retry(&state, |c| {
        let album_id = album_id.clone();
        async move {
            let tracks = c.album_tracks(&album_id).await?;
            to_queue_tracks(&c, tracks)
        }
    })
    .await?;
    state.player.send(if next {
        PlayerCommand::PlayNext(queue)
    } else {
        PlayerCommand::PlayLast(queue)
    })
}

/// Add individual tracks (by id) to the queue.
#[tauri::command]
pub async fn enqueue_tracks(
    state: State<'_, AppState>,
    track_ids: Vec<String>,
    next: bool,
) -> AppResult<()> {
    let queue = with_retry(&state, |c| {
        let track_ids = track_ids.clone();
        async move {
            let tracks = c.tracks_by_ids(&track_ids).await?;
            to_queue_tracks(&c, tracks)
        }
    })
    .await?;
    state.player.send(if next {
        PlayerCommand::PlayNext(queue)
    } else {
        PlayerCommand::PlayLast(queue)
    })
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

#[tauri::command]
pub fn get_queue(state: State<'_, AppState>) -> player::QueueSnapshot {
    state.player.queue_snapshot()
}

/// The visualizer opens a binary IPC channel; the player's PCM tap streams the
/// audible signal, folded to stereo, into it while subscribed.
#[tauri::command]
pub fn visualizer_subscribe(
    state: State<'_, AppState>,
    window: tauri::Window,
    channel: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) {
    // Remember which window this belongs to: closing the projector destroys
    // its WebView without the frontend getting to unsubscribe.
    state
        .player
        .tap()
        .subscribe(window.label().to_string(), channel);
}

#[tauri::command]
pub fn visualizer_unsubscribe(state: State<'_, AppState>, channel_id: u32) {
    state.player.tap().unsubscribe(channel_id);
}

/// Replace the queue with a server-generated mix seeded by any item.
#[tauri::command]
pub async fn play_instant_mix(state: State<'_, AppState>, item_id: String) -> AppResult<()> {
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
    state.player.send(PlayerCommand::PlayQueue {
        tracks: queue,
        start_index: 0,
    })
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
/// (`item_id`), or on whatever is playing when it is left out.
/// `StopAfter::Off` clears it.
#[tauri::command]
pub fn player_set_stop_after(
    state: State<'_, AppState>,
    mode: crate::player::StopAfter,
    item_id: Option<String>,
) -> AppResult<()> {
    state
        .player
        .send(PlayerCommand::SetStopAfter { mode, item_id })
}

fn listenbrainz_entry() -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(KEYRING_SERVICE, "listenbrainz")?)
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

/// The token itself never goes back to the WebView — the UI only learns
/// whether one is configured.
#[tauri::command]
pub fn get_extras_settings(state: State<'_, AppState>) -> player::ExtrasSettings {
    let mut settings: player::ExtrasSettings = state
        .store
        .get(crate::store::keys::EXTRAS)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    // Migrate a token that a pre-keyring build left in the store.
    if let Some(token) = settings.listenbrainz_token.take() {
        if !token.is_empty() {
            if let Ok(entry) = listenbrainz_entry() {
                if entry.set_password(&token).is_ok() {
                    if let Ok(json) = serde_json::to_string(&settings) {
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
    let json = serde_json::to_string(&settings)
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

/// Returns the list of imported keys; player settings apply after restart or
/// the next settings change.
#[tauri::command]
pub fn import_settings(
    state: State<'_, AppState>,
    cache: State<'_, CoverCache>,
    json: String,
) -> AppResult<Vec<String>> {
    let imported = state.store.import_settings(&json)?;
    // Push the freshly imported player-relevant settings live.
    if let Some(raw) = state.store.get(crate::store::keys::PLAYBACK) {
        if let Ok(settings) = serde_json::from_str(&raw) {
            let _ = state
                .player
                .send(PlayerCommand::SetPlaybackSettings(settings));
        }
    }
    if let Some(raw) = state.store.get(crate::store::keys::EXTRAS) {
        if let Ok(mut settings) = serde_json::from_str::<player::ExtrasSettings>(&raw) {
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

#[tauri::command]
pub fn set_playback_settings(
    state: State<'_, AppState>,
    settings: player::PlaybackSettings,
) -> AppResult<()> {
    let json = serde_json::to_string(&settings)
        .map_err(|e| AppError::Other(format!("cannot serialize settings: {e}")))?;
    state.store.set(crate::store::keys::PLAYBACK, &json)?;
    state
        .player
        .send(PlayerCommand::SetPlaybackSettings(settings))
}

#[tauri::command]
pub fn list_audio_devices() -> crate::audio_devices::AudioDeviceSnapshot {
    crate::audio_devices::snapshot()
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
            "availableDeviceCount": player::output_device_names().len(),
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
        write_unique(&dir, "jellysic-diagnostics.json", &bytes)
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
    let mut command = {
        let mut c = std::process::Command::new("explorer");
        c.arg(format!("/select,{path}"));
        c
    };
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
    state.store.set(crate::store::keys::AUDIO_DSP, &json)
}

/// Ask the library watcher to poll the server right now (fired when the
/// window regains focus, so a change made while away shows up immediately).
#[tauri::command]
pub fn check_library_now(state: State<'_, AppState>) {
    state.library_watch.check_now();
}

/// Whether the server is currently reachable (drives the offline banner).
/// True when not connected — the setup screen handles that case.
#[tauri::command]
pub async fn check_server(state: State<'_, AppState>) -> AppResult<bool> {
    let client = { state.session.read().await.clone() };
    Ok(match client {
        Some(c) => !matches!(c.validate().await, crate::api::SessionValidity::Unreachable),
        None => true,
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

#[tauri::command]
pub fn queue_remove(state: State<'_, AppState>, index: usize, item_id: String) -> AppResult<()> {
    state
        .player
        .send(PlayerCommand::RemoveAt { index, item_id })
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
    use super::{pin_after_login, relogin_after_401, sanitize_filename, write_unique};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    #[test]
    fn only_a_rejected_token_turns_a_401_into_a_relogin() {
        use crate::api::SessionValidity;
        // Still valid: the 401 was a permission failure, and a re-login would
        // only rotate the device token under a running stream.
        assert!(!relogin_after_401(SessionValidity::Valid));
        assert!(relogin_after_401(SessionValidity::Invalid));
        // The probe did not get through: no evidence the token is gone.
        assert!(!relogin_after_401(SessionValidity::Unreachable));
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
            std::env::temp_dir().join(format!("jellysic-download-test-{}", uuid::Uuid::new_v4()));
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
