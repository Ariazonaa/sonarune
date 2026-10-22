//! The renewal of a revoked session for work outside `with_retry`
//! ([`renew_after_401`]), and the re-login rules under it, against a session
//! host in memory and a stand-in server for the session check.

use super::{
    commit_restore, commit_sign_in, renew_after_401, Identify, SessionEndReason, SessionEnded,
    SessionHost, SignIn, StoredLogin, BACKGROUND_RELOGIN_PAUSE,
};
use crate::api::types::UserDto;
use crate::api::JellyfinClient;
use crate::error::{AppError, AppResult};
use crate::player::SharedSession;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A stand-in server: `/Users/Me` (the session check) answers `probe`, a
/// sign-out goes through. Counts both.
struct StandIn {
    url: String,
    probes: Arc<AtomicUsize>,
    logouts: Arc<AtomicUsize>,
}

fn stand_in(probe: &'static str) -> StandIn {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stand-in server");
    let url = format!("http://{}", listener.local_addr().expect("server address"));
    let probes = Arc::new(AtomicUsize::new(0));
    let logouts = Arc::new(AtomicUsize::new(0));
    let (probed, logged_out) = (probes.clone(), logouts.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let (probed, logged_out) = (probed.clone(), logged_out.clone());
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&buf[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&head);
                let path = head.split(' ').nth(1).unwrap_or_default();
                let status = if path.starts_with("/Users/Me") {
                    probed.fetch_add(1, Ordering::SeqCst);
                    probe
                } else if path.starts_with("/Sessions/Logout") {
                    logged_out.fetch_add(1, Ordering::SeqCst);
                    "204 No Content"
                } else {
                    "404 Not Found"
                };
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                );
            });
        }
    });
    StandIn {
        url,
        probes,
        logouts,
    }
}

/// What a sign-in with the stored password brings.
#[derive(Clone, Copy)]
enum Answer {
    /// It goes through, as this user id.
    As(&'static str),
    /// The server answers this status.
    Status(u16),
}

/// The app's side of a session, in memory: what `AppState` keeps in the
/// store, the Credential Manager and the UI.
struct Host {
    server_url: String,
    session: SharedSession,
    gate: tokio::sync::Mutex<()>,
    blocked: AtomicBool,
    /// Server and user id the store names as signed in.
    signed_in: Mutex<Option<(String, String)>>,
    password: Mutex<Option<String>>,
    answer: Mutex<Answer>,
    /// How long a sign-in is on the wire.
    sign_in_takes: Duration,
    /// The session ends (from elsewhere) while the sign-in is on the wire.
    end_during_sign_in: AtomicBool,
    /// How long keeping a login takes (the store and the Credential Manager).
    keep_login_takes: Mutex<Duration>,
    /// Told when a re-login starts keeping its login.
    keeping: tokio::sync::Notify,
    /// Told when someone is about to take the gate.
    at_gate: tokio::sync::Notify,
    /// The id the server says when asked (`identify`), and every ask.
    says_id: Mutex<Option<String>>,
    identified: Mutex<Vec<Identify>>,
    sign_ins: AtomicUsize,
    forgotten: AtomicUsize,
    ended: Mutex<Vec<SessionEnded>>,
}

const ALICE: &str = "alice-id";
const BOB: &str = "bob-id";

fn client_of(url: &str, user_id: &str, token: &str) -> JellyfinClient {
    JellyfinClient::new(url, "test-device", Vec::new())
        .expect("client")
        .with_session(token.into(), user_id.into())
}

impl Host {
    /// Alice signed in on `server`, her session `stale` installed, her
    /// password stored; a sign-in goes through as her.
    fn new(server: &StandIn) -> (Arc<Self>, Arc<JellyfinClient>) {
        let stale = Arc::new(client_of(&server.url, ALICE, "revoked"));
        let host = Arc::new(Self {
            server_url: server.url.clone(),
            session: Arc::new(tokio::sync::RwLock::new(Some(stale.clone()))),
            gate: tokio::sync::Mutex::new(()),
            blocked: AtomicBool::new(false),
            signed_in: Mutex::new(Some((server.url.clone(), ALICE.into()))),
            password: Mutex::new(Some("secret".into())),
            answer: Mutex::new(Answer::As(ALICE)),
            sign_in_takes: Duration::from_millis(100),
            end_during_sign_in: AtomicBool::new(false),
            keep_login_takes: Mutex::new(Duration::ZERO),
            keeping: tokio::sync::Notify::new(),
            at_gate: tokio::sync::Notify::new(),
            says_id: Mutex::new(None),
            identified: Mutex::new(Vec::new()),
            sign_ins: AtomicUsize::new(0),
            forgotten: AtomicUsize::new(0),
            ended: Mutex::new(Vec::new()),
        });
        (host, stale)
    }

    fn answering(self: &Arc<Self>, answer: Answer) -> &Arc<Self> {
        *self.answer.lock().unwrap() = answer;
        self
    }

    async fn current(&self) -> Option<Arc<JellyfinClient>> {
        self.session.read().await.clone()
    }

    fn sign_ins(&self) -> usize {
        self.sign_ins.load(Ordering::SeqCst)
    }

    fn ended(&self) -> Vec<SessionEnded> {
        self.ended.lock().unwrap().clone()
    }
}

impl SessionHost for Host {
    fn session(&self) -> &SharedSession {
        &self.session
    }

    fn relogin_gate(&self) -> &tokio::sync::Mutex<()> {
        self.at_gate.notify_one();
        &self.gate
    }

    fn relogin_blocked(&self) -> &AtomicBool {
        &self.blocked
    }

    fn signed_in(&self) -> Option<(String, String)> {
        self.signed_in.lock().unwrap().clone()
    }

    fn stored_login(&self) -> AppResult<Option<StoredLogin>> {
        Ok(Some(StoredLogin {
            server_url: self.server_url.clone(),
            username: "alice".into(),
            password: self.password.lock().unwrap().clone(),
        }))
    }

    async fn sign_in(
        &self,
        login: &StoredLogin,
        _password: &str,
    ) -> AppResult<(SignIn, AppResult<UserDto>)> {
        let n = self.sign_ins.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.sign_in_takes).await;
        if self.end_during_sign_in.load(Ordering::SeqCst) {
            *self.session.write().await = None;
        }
        let answer = *self.answer.lock().unwrap();
        let (client, result) = match answer {
            Answer::As(user_id) => (
                client_of(&login.server_url, user_id, &format!("fresh-{n}")),
                Ok(
                    serde_json::from_value(serde_json::json!({ "Id": user_id, "Name": "alice" }))
                        .expect("user"),
                ),
            ),
            Answer::Status(status) => (
                JellyfinClient::new(&login.server_url, "test-device", Vec::new())?,
                Err(AppError::Server {
                    status,
                    message: "refused".into(),
                }),
            ),
        };
        let sign_in = SignIn {
            client,
            candidate: None,
            auto_trust: false,
            device_id: "test-device".into(),
        };
        Ok((sign_in, result))
    }

    /// `commands::identify`'s fallbacks, without the store: the id the
    /// server says, else the renewed session's.
    async fn identify(&self, _client: &JellyfinClient, how: Identify) -> Option<String> {
        self.identified.lock().unwrap().push(how.clone());
        let said = self.says_id.lock().unwrap().clone();
        match how {
            Identify::Renewal(previous) => said.or(previous),
            Identify::SignIn => said,
            Identify::Offline => None,
        }
    }

    fn forget_password(&self, _login: &StoredLogin) {
        self.forgotten.fetch_add(1, Ordering::SeqCst);
        *self.password.lock().unwrap() = None;
    }

    fn keep_login(
        &self,
        login: &StoredLogin,
        sign_in: SignIn,
        user: &UserDto,
    ) -> AppResult<JellyfinClient> {
        let takes = *self.keep_login_takes.lock().unwrap();
        self.keeping.notify_one();
        std::thread::sleep(takes);
        *self.signed_in.lock().unwrap() = Some((login.server_url.clone(), user.id.clone()));
        Ok(sign_in.client)
    }

    async fn install(
        &self,
        mut client: JellyfinClient,
        server_id: Option<String>,
    ) -> Arc<JellyfinClient> {
        client.set_server_id(server_id);
        let client = Arc::new(client);
        self.blocked.store(false, Ordering::Relaxed);
        *self.session.write().await = Some(client.clone());
        client
    }

    fn announce_end(&self, ended: SessionEnded) {
        self.ended.lock().unwrap().push(ended);
    }
}

/// The error a renewal gave up with.
fn failure(result: AppResult<Arc<JellyfinClient>>) -> AppError {
    match result {
        Ok(_) => panic!("the session was renewed"),
        Err(err) => err,
    }
}

fn unauthorized() -> AppError {
    AppError::Server {
        status: 401,
        message: String::new(),
    }
}

/// `callers` renewals for 401s on `stale` at `at`, side by side.
async fn renew_side_by_side(
    host: &Arc<Host>,
    stale: &Arc<JellyfinClient>,
    at: Instant,
    callers: usize,
) -> Vec<AppResult<Arc<JellyfinClient>>> {
    let tasks: Vec<_> = (0..callers)
        .map(|_| {
            let (host, stale) = (host.clone(), stale.clone());
            tokio::spawn(async move { renew_after_401(&*host, &stale, at, unauthorized()).await })
        })
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.expect("renewal task"));
    }
    results
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_session_is_renewed_once_and_handed_over() {
    let server = stand_in("401 Unauthorized");
    let (host, stale) = Host::new(&server);

    let renewed = renew_after_401(&*host, &stale, Instant::now(), unauthorized())
        .await
        .expect("renewed");
    assert!(!Arc::ptr_eq(&renewed, &stale));
    assert_eq!(renewed.user_id().ok(), Some(ALICE));
    assert_eq!(renewed.token(), Some("fresh-0"));
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &renewed));
    assert_eq!(server.probes.load(Ordering::SeqCst), 1);
    assert_eq!(host.sign_ins(), 1);
    assert!(host.ended().is_empty());

    // A 401 of the old session that comes back late is retried on the
    // renewed one, without a check or a login of its own.
    let late = renew_after_401(&*host, &stale, Instant::now(), unauthorized())
        .await
        .expect("handed the renewed session");
    assert!(Arc::ptr_eq(&late, &renewed));
    assert_eq!(server.probes.load(Ordering::SeqCst), 1);
    assert_eq!(host.sign_ins(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_401s_share_one_renewal() {
    let server = stand_in("401 Unauthorized");
    let (host, stale) = Host::new(&server);

    // A track open, a prefetch, the library watch and downloads, at once.
    let results = renew_side_by_side(&host, &stale, Instant::now(), 8).await;
    let renewed = host.current().await.expect("renewed session");
    for result in results {
        assert!(Arc::ptr_eq(&result.expect("renewed"), &renewed));
    }
    assert_eq!(server.probes.load(Ordering::SeqCst), 1, "one session check");
    assert_eq!(host.sign_ins(), 1, "one login");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_is_renewed_after_a_sign_out() {
    let server = stand_in("401 Unauthorized");
    let (host, stale) = Host::new(&server);
    *host.session.write().await = None;

    let result = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(result);
    assert!(matches!(err, AppError::NotConnected), "{err:?}");
    assert_eq!(server.probes.load(Ordering::SeqCst), 0);
    assert_eq!(host.sign_ins(), 0);
    assert!(host.current().await.is_none());
    assert!(host.ended().is_empty(), "the sign-out said all there is");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renewal_racing_a_sign_out_does_not_bring_the_session_back() {
    let server = stand_in("401 Unauthorized");
    let (host, stale) = Host::new(&server);

    // A sign-out holds the gate while the 401 comes in.
    let signing_out = host.gate.lock().await;
    let renewal = {
        let (host, stale) = (host.clone(), stale.clone());
        tokio::spawn(async move {
            renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await
        })
    };
    // The re-login is at the gate: the sign-out finishes.
    host.at_gate.notified().await;
    *host.session.write().await = None;
    drop(signing_out);

    let result = renewal.await.expect("renewal task");
    let err = failure(result);
    assert!(matches!(err, AppError::NotConnected), "{err:?}");
    assert_eq!(host.sign_ins(), 0);
    assert!(host.current().await.is_none());
    assert!(host.ended().is_empty());

    // The session ends while the login is on the wire: the fresh session is
    // signed out again, never installed.
    let (host, stale) = Host::new(&server);
    host.end_during_sign_in.store(true, Ordering::SeqCst);
    let result = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(result);
    assert!(matches!(err, AppError::NotConnected), "{err:?}");
    assert_eq!(host.sign_ins(), 1);
    assert!(host.current().await.is_none());
    assert_eq!(server.logouts.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_that_is_not_signed_in_any_more_is_not_renewed() {
    let server = stand_in("401 Unauthorized");

    // Another account signed in while the call was out.
    let (host, stale) = Host::new(&server);
    let bob = Arc::new(client_of(&server.url, BOB, "bobs"));
    *host.session.write().await = Some(bob.clone());
    let result = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(result);
    assert!(matches!(err, AppError::NotConnected), "{err:?}");
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &bob));

    // Bob's sign-in is under way: the store names him, the old session is
    // still installed (`forget_previous_login` ran).
    let (host, stale) = Host::new(&server);
    *host.signed_in.lock().unwrap() = Some((server.url.clone(), BOB.into()));
    let result = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(result);
    assert!(matches!(err, AppError::NotConnected), "{err:?}");
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &stale));
    assert_eq!(host.sign_ins(), 0);
    assert_eq!(server.probes.load(Ordering::SeqCst), 0);

    // The login itself comes back as another account: it is not handed to
    // work started for Alice.
    let (host, stale) = Host::new(&server);
    host.answering(Answer::As(BOB));
    let result = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(result);
    assert!(matches!(err, AppError::NotConnected), "{err:?}");
    assert_eq!(host.sign_ins(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_renewal_ends_the_session_once() {
    for (status, password_dropped) in [(401, true), (403, false)] {
        let server = stand_in("401 Unauthorized");
        let (host, stale) = Host::new(&server);
        host.answering(Answer::Status(status));

        let results = renew_side_by_side(&host, &stale, Instant::now(), 4).await;
        for result in results {
            assert!(result.is_err());
        }
        assert_eq!(host.sign_ins(), 1, "{status}: one login");
        assert_eq!(
            host.ended(),
            vec![SessionEnded {
                reason: SessionEndReason::Rejected,
                server_url: server.url.clone(),
                username: "alice".into(),
            }],
            "{status}: said once"
        );
        assert!(host.current().await.is_none());
        assert!(host.blocked.load(Ordering::Relaxed));
        assert_eq!(
            host.forgotten.load(Ordering::SeqCst),
            usize::from(password_dropped),
            "{status}"
        );

        // Whatever comes after finds no session to renew.
        let late = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
        let err = failure(late);
        assert!(matches!(err, AppError::NotConnected), "{err:?}");
        assert_eq!(host.sign_ins(), 1);
        assert_eq!(host.ended().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_renewal_answers_background_401s_for_a_while() {
    let server = stand_in("401 Unauthorized");
    let (host, stale) = Host::new(&server);
    host.answering(Answer::Status(503));

    let first = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(first);
    assert!(
        matches!(err, AppError::Server { status: 503, .. }),
        "{err:?}"
    );
    // A 401 that came after that attempt, within the pause: its answer, no
    // second check and no second login.
    let later = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(later);
    assert!(
        matches!(err, AppError::Server { status: 503, .. }),
        "{err:?}"
    );
    assert_eq!(host.sign_ins(), 1);
    assert_eq!(server.probes.load(Ordering::SeqCst), 1);
    // Not a rejection: the session stays, nothing is said.
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &stale));
    assert!(host.ended().is_empty());
    assert!(!host.blocked.load(Ordering::Relaxed));

    // Past the pause, the server is asked again.
    host.answering(Answer::As(ALICE));
    let much_later = Instant::now() + BACKGROUND_RELOGIN_PAUSE + Duration::from_secs(1);
    let renewed = renew_after_401(&*host, &stale, much_later, unauthorized()).await;
    assert!(renewed.is_ok(), "{:?}", renewed.err());
    assert_eq!(host.sign_ins(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_401_the_session_check_does_not_confirm_is_left_alone() {
    // Jellyfin answers some permission failures with 401 too.
    let server = stand_in("200 OK");
    let (host, stale) = Host::new(&server);
    let result = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    let err = failure(result);
    assert!(
        matches!(err, AppError::Server { status: 401, .. }),
        "{err:?}"
    );
    assert_eq!(server.probes.load(Ordering::SeqCst), 1);
    assert_eq!(host.sign_ins(), 0);
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &stale));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sign_in_waits_for_a_relogin_that_is_keeping_its_login() {
    let server = stand_in("401 Unauthorized");
    let (host, stale) = Host::new(&server);
    // The re-login has its answer and is storing it when Bob signs in.
    *host.keep_login_takes.lock().unwrap() = Duration::from_millis(400);
    let renewal = {
        let (host, stale) = (host.clone(), stale.clone());
        tokio::spawn(async move {
            renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await
        })
    };
    host.keeping.notified().await;
    let bob = commit_sign_in(&*host, None, || {
        *host.signed_in.lock().unwrap() = Some((server.url.clone(), BOB.into()));
        Ok(client_of(&server.url, BOB, "bobs"))
    })
    .await
    .expect("signed in");

    // Alice's renewal finished first, and nothing of hers came after Bob's.
    let renewed = renewal.await.expect("renewal task").expect("renewed");
    assert_eq!(renewed.user_id().ok(), Some(ALICE));
    assert_eq!(host.sign_ins(), 1);
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &bob));
    assert_eq!(
        host.signed_in(),
        Some((server.url.clone(), BOB.to_string()))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_installs_only_over_what_it_found() {
    let server = stand_in("401 Unauthorized");
    let alice = (server.url.as_str(), ALICE);
    let restored = || Ok(client_of(&server.url, ALICE, "stored"));

    // Signed out (the session ended), Alice still stored: hers comes back.
    let (host, _) = Host::new(&server);
    *host.session.write().await = None;
    let back = commit_restore(&*host, alice, None, None, restored)
        .await
        .unwrap()
        .expect("restored");
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &back));

    // What the restore found installed, and still is, is replaced.
    let (host, stale) = Host::new(&server);
    let back = commit_restore(&*host, alice, Some(&stale), None, restored)
        .await
        .unwrap()
        .expect("restored");
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &back));

    // Bob signed in while the restore was validating: his session stays.
    let (host, _) = Host::new(&server);
    let bob = Arc::new(client_of(&server.url, BOB, "bobs"));
    *host.session.write().await = Some(bob.clone());
    *host.signed_in.lock().unwrap() = Some((server.url.clone(), BOB.into()));
    let kept = AtomicBool::new(false);
    let back = commit_restore(&*host, alice, None, None, || {
        kept.store(true, Ordering::SeqCst);
        restored()
    })
    .await
    .unwrap();
    assert!(back.is_none());
    assert!(!kept.load(Ordering::SeqCst), "nothing of Alice's is stored");
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &bob));

    // Alice is still stored, but a re-login installed a session meanwhile.
    let (host, _) = Host::new(&server);
    let renewed = Arc::new(client_of(&server.url, ALICE, "fresh"));
    *host.session.write().await = Some(renewed.clone());
    let back = commit_restore(&*host, alice, None, None, restored)
        .await
        .unwrap();
    assert!(back.is_none());
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &renewed));

    // Signed out meanwhile: nothing comes back.
    let (host, _) = Host::new(&server);
    *host.session.write().await = None;
    *host.signed_in.lock().unwrap() = None;
    let back = commit_restore(&*host, alice, None, None, restored)
        .await
        .unwrap();
    assert!(back.is_none());
    assert!(host.current().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_waits_for_a_sign_in_that_is_storing_its_login() {
    let server = stand_in("401 Unauthorized");
    let (host, _) = Host::new(&server);
    *host.session.write().await = None;
    let storing = Arc::new(tokio::sync::Notify::new());

    // Bob's sign-in has its answer and is storing it.
    let signing_in = {
        let (host, storing, url) = (host.clone(), storing.clone(), server.url.clone());
        tokio::spawn(async move {
            commit_sign_in(&*host, None, || {
                storing.notify_one();
                std::thread::sleep(Duration::from_millis(300));
                *host.signed_in.lock().unwrap() = Some((url.clone(), BOB.into()));
                Ok(client_of(&url, BOB, "bobs"))
            })
            .await
        })
    };
    storing.notified().await;
    // Alice's restore, which read her account before, comes back now.
    let kept = AtomicBool::new(false);
    let back = commit_restore(&*host, (&server.url, ALICE), None, None, || {
        kept.store(true, Ordering::SeqCst);
        Ok(client_of(&server.url, ALICE, "stored"))
    })
    .await
    .unwrap();

    let bob = signing_in.await.expect("sign-in task").expect("signed in");
    assert!(back.is_none());
    assert!(!kept.load(Ordering::SeqCst));
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &bob));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renewed_session_keeps_its_server_when_the_server_does_not_say() {
    let server = stand_in("401 Unauthorized");
    let (host, _) = Host::new(&server);
    let mut stale = client_of(&server.url, ALICE, "revoked");
    stale.set_server_id(Some("server-a".into()));
    let stale = Arc::new(stale);
    *host.session.write().await = Some(stale.clone());

    let renewed = renew_after_401(&*host, &stale, Instant::now(), unauthorized())
        .await
        .expect("renewed");
    // Asked once, after the login and before the session was installed,
    // with the renewed session's server to fall back on.
    assert_eq!(
        *host.identified.lock().unwrap(),
        [Identify::Renewal(Some("server-a".into()))]
    );
    assert_eq!(renewed.server_id(), Some("server-a"));

    // A server that says its id is taken at its word.
    let (host, stale) = Host::new(&server);
    *host.says_id.lock().unwrap() = Some("server-b".into());
    let renewed = renew_after_401(&*host, &stale, Instant::now(), unauthorized())
        .await
        .expect("renewed");
    assert_eq!(renewed.server_id(), Some("server-b"));

    // A login the server refused asks nothing.
    let (host, stale) = Host::new(&server);
    host.answering(Answer::Status(503));
    let _ = renew_after_401(&*host, &stale, Instant::now(), unauthorized()).await;
    assert!(host.identified.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sign_in_and_a_restore_install_the_server_they_found() {
    let server = stand_in("401 Unauthorized");
    let (host, _) = Host::new(&server);
    *host.session.write().await = None;
    let back = commit_restore(
        &*host,
        (&server.url, ALICE),
        None,
        Some("server-a".into()),
        || Ok(client_of(&server.url, ALICE, "stored")),
    )
    .await
    .unwrap()
    .expect("restored");
    assert_eq!(back.server_id(), Some("server-a"));

    let bob = commit_sign_in(&*host, Some("server-b".into()), || {
        Ok(client_of(&server.url, BOB, "bobs"))
    })
    .await
    .unwrap();
    assert_eq!(bob.server_id(), Some("server-b"));
    assert!(Arc::ptr_eq(&host.current().await.unwrap(), &bob));
}
