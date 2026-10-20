//! The client against a local stand-in server: what goes out on the wire, and
//! how answers are read and classified.

use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;

/// One request as the stand-in saw it.
#[derive(Debug, Clone)]
struct Request {
    method: String,
    /// Path and query, as sent.
    target: String,
    /// Names lowercased.
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// A query parameter, raw (the tests only use plain ids and numbers).
    fn query(&self, name: &str) -> Option<&str> {
        let (_, query) = self.target.split_once('?')?;
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
    }
}

struct Reply {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    delay: Duration,
}

impl Reply {
    fn status(status: &'static str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    fn json(body: serde_json::Value) -> Self {
        Self::status("200 OK")
            .header("Content-Type", "application/json")
            .body(body.to_string().into_bytes())
    }

    fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// A local HTTP server answering every request with `handler`, one thread per
/// connection so concurrent requests really run side by side. Returns its URL.
fn serve(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stand-in server");
    let url = format!("http://{}", listener.local_addr().expect("server address"));
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let handler = handler.clone();
            std::thread::spawn(move || {
                let Some(request) = read_request(&mut stream) else {
                    return;
                };
                let reply = handler(&request);
                std::thread::sleep(reply.delay);
                let _ = write_reply(&mut stream, &reply);
            });
        }
    });
    url
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut buf).ok()?;
        if read == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..read]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    // Read the body too: closing on unread data would reset the connection
    // under the reply.
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body_read = data.len() - head_end;
    while body_read < length {
        let read = stream.read(&mut buf).ok()?;
        if read == 0 {
            break;
        }
        body_read += read;
    }
    Some(Request {
        method,
        target,
        headers,
    })
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)
}

/// The address of a port nothing listens on.
fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    drop(listener);
    url
}

fn session(url: &str) -> JellyfinClient {
    JellyfinClient::new(url, "test-device", Vec::new())
        .expect("client")
        .with_session("token".into(), "user".into())
}

/// `count` audio items with ids `{prefix}{n}`, `n` from `first`.
fn audio_items(prefix: &str, first: usize, count: usize) -> Vec<serde_json::Value> {
    (first..first + count)
        .map(|n| serde_json::json!({ "Id": format!("{prefix}{n}"), "Type": "Audio" }))
        .collect()
}

fn items_page(items: Vec<serde_json::Value>, total: usize) -> Reply {
    Reply::json(serde_json::json!({ "Items": items, "TotalRecordCount": total }))
}

#[tokio::test]
async fn every_request_names_the_client() {
    let seen = Arc::new(Mutex::new(None::<String>));
    let record = seen.clone();
    let url = serve(move |request| {
        *record.lock().unwrap() = request.header("user-agent").map(str::to_string);
        Reply::status("200 OK")
    });
    assert_eq!(session(&url).validate().await, SessionValidity::Valid);
    // Without one, Cloudflare's default Browser Integrity Check answers 403.
    assert_eq!(seen.lock().unwrap().as_deref(), Some(USER_AGENT));
    assert_eq!(USER_AGENT, format!("{CLIENT_NAME}/{CLIENT_VERSION}"));
}

#[test]
fn an_upgrade_to_https_on_the_same_host_names_the_address_to_use() {
    let target = |url: &str| reqwest::Url::parse(url).unwrap();
    let me = "/Users/Me";
    // The proxy keeps the request path: the API path comes off, a sub-path
    // the server lives under stays.
    assert_eq!(
        upgraded_base_url(
            "http://music.home/jellyfin",
            me,
            &target("https://music.home/jellyfin/Users/Me")
        )
        .as_deref(),
        Some("https://music.home/jellyfin")
    );
    assert_eq!(
        upgraded_base_url(
            "http://music.home:8096",
            me,
            &target("https://music.home/Users/Me")
        )
        .as_deref(),
        Some("https://music.home")
    );
    assert_eq!(
        upgraded_base_url(
            "http://music.home",
            me,
            &target("https://music.home:8920/Users/Me?x=1")
        )
        .as_deref(),
        Some("https://music.home:8920")
    );
    // Sent to the root instead: same path as before, now on https.
    assert_eq!(
        upgraded_base_url(
            "http://music.home/jellyfin",
            me,
            &target("https://music.home/")
        )
        .as_deref(),
        Some("https://music.home/jellyfin")
    );
    // Another host may be a captive portal; never suggested.
    assert_eq!(
        upgraded_base_url(
            "http://music.home",
            me,
            &target("https://portal.hotel/login")
        ),
        None
    );
    // A downgrade is not an address to move to.
    assert_eq!(
        upgraded_base_url(
            "https://music.home:8920",
            me,
            &target("http://music.home/Users/Me")
        ),
        None
    );
}

/// A stand-in that sends every request to `location` with a 308.
fn redirecting(location: impl Fn(&Request) -> String + Send + Sync + 'static) -> String {
    serve(move |request| {
        Reply::status("308 Permanent Redirect").header("Location", location(request))
    })
}

#[tokio::test]
async fn a_sign_in_that_is_redirected_to_https_says_where_to_sign_in() {
    let url = redirecting(|request| format!("https://127.0.0.1{}", request.target));
    let mut client =
        JellyfinClient::new(&format!("{url}/jellyfin"), "test-device", Vec::new()).expect("client");
    let err = client.authenticate("alice", "secret").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "server returned 308: the server redirects to https://127.0.0.1/jellyfin — sign in with that address"
    );

    // A stored http address the server now upgrades: moved, not offline.
    let client = session(&format!("{url}/jellyfin"));
    assert_eq!(
        client.validate().await,
        SessionValidity::Moved("https://127.0.0.1/jellyfin".into())
    );
}

#[tokio::test]
async fn a_redirect_elsewhere_is_named_but_never_followed_or_suggested() {
    let url = redirecting(|_| {
        "https://portal.hotel.example/login?next=%2FQuickConnect%3Fsecret%3Dabc".into()
    });
    let client = session(&url);
    // Could be a captive portal: the server is out of reach, nothing more.
    assert_eq!(client.validate().await, SessionValidity::Unreachable);
    let err = client.me().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "server returned 308: the server redirects to https://portal.hotel.example (not followed)"
    );

    let mut client = JellyfinClient::new(&url, "test-device", Vec::new()).expect("client");
    let err = client.authenticate("alice", "secret").await.unwrap_err();
    assert!(!err.to_string().contains("sign in with"), "{err}");
    assert!(!err.to_string().contains("secret"), "{err}");
}

/// A stand-in on `listener` that hangs up on every request without an answer
/// — what a handshake the client refuses ends in — after `on_connection` ran,
/// which plays the verifier's part: recording the leaf that was presented.
fn hanging_up(listener: TcpListener, on_connection: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            on_connection();
            let _ = read_request(&mut stream);
        }
    });
}

#[tokio::test]
async fn a_rejected_certificate_is_not_reported_as_an_outage() {
    let pinned = crate::tls::fingerprint(b"pinned");
    let renewed = crate::tls::fingerprint(b"renewed");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stand-in server");
    let url = format!("http://{}", listener.local_addr().expect("server address"));
    let client = JellyfinClient::new(&url, "test-device", vec![pinned.clone()])
        .expect("client")
        .with_session("token".into(), "user".into());
    // The leaf the next handshake presents.
    let presents = Arc::new(Mutex::new(Some(renewed.clone())));
    let (observed, presented) = (client.observed.clone(), presents.clone());
    hanging_up(listener, move || {
        *observed.lock().unwrap() = presented.lock().unwrap().clone();
    });

    // The handshake made for this probe was refused on the renewed leaf.
    assert_eq!(
        client.validate().await,
        SessionValidity::Untrusted(renewed.clone())
    );
    // The pinned certificate itself, or a public one: just the network.
    *presents.lock().unwrap() = Some(pinned);
    assert_eq!(client.validate().await, SessionValidity::Unreachable);
    *presents.lock().unwrap() = None;
    assert_eq!(client.validate().await, SessionValidity::Unreachable);
}

#[tokio::test]
async fn a_certificate_seen_earlier_does_not_turn_an_outage_into_one() {
    // A captive portal's certificate, recorded on a network the device has
    // left since. Now nothing answers at all: no handshake, no certificate.
    let client = session(&closed_port_url());
    *client.observed.lock().unwrap() = Some(crate::tls::fingerprint(b"portal"));
    assert_eq!(client.validate().await, SessionValidity::Unreachable);
    // Gone for the trust prompt as well.
    assert_eq!(client.untrusted_fingerprint(), None);
}

/// A client like the one `trust_session_certificate` probes with: it accepts
/// the confirmed certificate, carries the session's token, and its handshake
/// presented `presented` (recorded the way the verifier does).
fn trusting(url: &str, confirmed: &str, presented: Option<&str>) -> JellyfinClient {
    let client = JellyfinClient::new(url, "test-device", vec![confirmed.to_string()])
        .expect("client")
        .with_session("token".into(), "user-1".into());
    *client.observed.lock().unwrap() = presented.map(str::to_string);
    client
}

#[tokio::test]
async fn a_certificate_is_pinned_only_on_the_sessions_own_account() {
    use crate::commands::{check_confirmed_certificate, CertificateCheck};

    let confirmed = crate::tls::fingerprint(b"renewed");
    let check = |url: String, presented: Option<String>| {
        let confirmed = confirmed.clone();
        async move {
            let client = trusting(&url, &confirmed, presented.as_deref());
            check_confirmed_certificate(&client, &confirmed).await
        }
    };
    let account = |id: &'static str| {
        serve(move |request| {
            if request.path() == "/Users/Me" {
                Reply::json(serde_json::json!({ "Id": id, "Name": "alice", "Policy": {} }))
            } else {
                Reply::status("404 Not Found")
            }
        })
    };
    let on_confirmed = || Some(confirmed.clone());

    // A captive portal or an interception: 200 to everything, with a page of
    // its own. It got the confirmed certificate accepted — and pins nothing.
    let portal = serve(|_| {
        Reply::status("200 OK")
            .header("Content-Type", "text/html")
            .body("<html><body>Please sign in to the hotel Wi-Fi</body></html>")
    });
    let err = check(portal.clone(), on_confirmed()).await.unwrap_err();
    assert!(
        err.to_string().starts_with("unreadable server response"),
        "{err}"
    );
    // The session probe alone would have taken that 200 for a working
    // session: it looks at the status only.
    assert_eq!(
        trusting(&portal, &confirmed, None).validate().await,
        SessionValidity::Valid
    );
    // JSON, but no user.
    let not_a_user = serve(|_| Reply::json(serde_json::json!({ "status": "ok" })));
    assert!(check(not_a_user, on_confirmed()).await.is_err());
    // A user, but not this session's.
    let err = check(account("user-2"), on_confirmed()).await.unwrap_err();
    assert_eq!(err.to_string(), "the server answered for another account");

    // The session's own account, on exactly the confirmed certificate.
    assert_eq!(
        check(account("user-1"), on_confirmed()).await.unwrap(),
        CertificateCheck::Pin(Some(confirmed.clone()))
    );
    // On a publicly valid one, or one pinned before: nothing new to pin.
    assert_eq!(
        check(account("user-1"), None).await.unwrap(),
        CertificateCheck::Pin(None)
    );
    let earlier = crate::tls::fingerprint(b"pinned");
    assert_eq!(
        check(account("user-1"), Some(earlier)).await.unwrap(),
        CertificateCheck::Pin(None)
    );

    // The token refused through it: a full sign-in is needed.
    for status in ["401 Unauthorized", "403 Forbidden"] {
        let refusing = serve(move |_| Reply::status(status));
        assert_eq!(
            check(refusing, on_confirmed()).await.unwrap(),
            CertificateCheck::Rejected,
            "{status}"
        );
    }
    // Nothing answers, or something presents yet another certificate.
    let err = check(closed_port_url(), None).await.unwrap_err();
    assert_eq!(err.to_string(), "the server cannot be reached");
    let other = crate::tls::fingerprint(b"other");
    let err = check(closed_port_url(), Some(other)).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "the server presents a different certificate now"
    );
}

#[test]
fn a_smart_scan_keeps_each_track_once() {
    let filter = SmartFilter {
        min_play_count: Some(1),
        ..SmartFilter::default()
    };
    let played = |id: &str, plays: i32| BaseItem {
        id: id.into(),
        user_data: Some(UserDataWire {
            play_count: plays,
            ..UserDataWire::default()
        }),
        ..BaseItem::default()
    };
    let mut matches = SmartMatches::default();
    matches.add([played("a", 1), played("b", 0), played("c", 2)], &filter);
    // A track added mid-scan shifted the next page: "c" comes back.
    matches.add([played("c", 2), played("d", 5)], &filter);
    let ids: Vec<&str> = matches.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["a", "c", "d"]);
}

#[tokio::test]
async fn a_track_shifted_onto_the_next_scan_page_is_not_played_twice() {
    let batch = SMART_SCAN_BATCH as usize;
    // A new track arrives at the top while the scan runs: the second page
    // starts one track early.
    let url = serve(move |request| {
        match request
            .query("startIndex")
            .and_then(|v| v.parse::<usize>().ok())
        {
            Some(0) => items_page(audio_items("t", 0, batch), 2 * batch),
            Some(start) if start == batch => {
                items_page(audio_items("t", batch - 1, batch), 2 * batch + 1)
            }
            Some(_) => items_page(audio_items("t", 2 * batch - 1, 1), 2 * batch + 1),
            None => Reply::status("400 Bad Request"),
        }
    });
    let tracks = session(&url)
        .smart_tracks(&SmartFilter::default())
        .await
        .expect("smart tracks");
    let unique: HashSet<&str> = tracks.iter().map(|track| track.id.as_str()).collect();
    assert_eq!(tracks.len(), 2 * batch);
    assert_eq!(unique.len(), tracks.len());
}

#[test]
fn only_the_newest_scan_writes_the_cache() {
    let tickets = ScanTickets::default();
    let slot: Mutex<Option<ScanCache<&str>>> = Mutex::new(None);
    let older = tickets.take();
    let newer = tickets.take();
    assert!(!tickets.is_current(older));
    assert!(tickets.is_current(newer));

    // The superseded scan finishes last: it must not replace the newer view.
    cache_scan(&slot, &tickets, newer, "newer", Arc::new(vec!["n".into()]));
    cache_scan(&slot, &tickets, older, "older", Arc::new(vec!["o".into()]));
    assert_eq!(
        slot.lock().unwrap().as_ref().map(|cache| cache.key),
        Some("newer")
    );
}

#[tokio::test]
async fn a_superseded_scan_stops_before_its_next_page() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let batch = SMART_SCAN_BATCH as usize;
    let url = serve(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        items_page(audio_items("t", 0, batch), 10 * batch)
    });
    let client = session(&url);

    // Superseded once the first page is in: no second request.
    let checks = AtomicUsize::new(0);
    let err = client
        .smart_track_items_while(&SmartFilter::default(), &|| {
            checks.fetch_add(1, Ordering::SeqCst) == 0
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), superseded().to_string());
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // The album grid's scans stop the same way, here before any page.
    let err = client
        .scan_items(&[("userId", "user".to_string())], |_| {}, &|| false)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), superseded().to_string());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn album_tracks_load_side_by_side_and_keep_the_callers_order() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (now, peak) = (in_flight.clone(), most.clone());
    let url = serve(move |request| {
        let album = request.query("parentId").unwrap_or_default().to_string();
        let running = now.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(running, Ordering::SeqCst);
        // The first albums answer last.
        let index: u64 = album.trim_start_matches('a').parse().unwrap_or(0);
        std::thread::sleep(Duration::from_millis(150 - index * 15));
        now.fetch_sub(1, Ordering::SeqCst);
        items_page(audio_items(&format!("{album}-"), 1, 2), 2)
    });
    let albums: Vec<String> = (0..8).map(|n| format!("a{n}")).collect();
    let tracks = session(&url)
        .tracks_for_albums(&albums)
        .await
        .expect("tracks");
    let ids: Vec<&str> = tracks.iter().map(|track| track.id.as_str()).collect();
    let expected: Vec<String> = (0..8)
        .flat_map(|n| [format!("a{n}-1"), format!("a{n}-2")])
        .collect();
    assert_eq!(ids, expected);
    let peak = most.load(Ordering::SeqCst);
    assert!(
        (2..=ALBUM_TRACKS_IN_FLIGHT).contains(&peak),
        "{peak} requests at once"
    );

    let err = session(&url)
        .tracks_for_albums(&["a1".into(), " ".into()])
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "album id must not be empty");
}

#[test]
fn creating_a_long_playlist_may_take_longer() {
    assert_eq!(create_playlist_timeout(0), Duration::from_secs(30));
    assert_eq!(create_playlist_timeout(100), Duration::from_secs(31));
    // A 15k-track smart view saved on a NAS.
    assert_eq!(create_playlist_timeout(15_000), Duration::from_secs(180));
    assert!(create_playlist_timeout(usize::MAX) > Duration::from_secs(30));
}

#[tokio::test]
async fn an_item_gone_from_the_server_is_a_404() {
    let url = serve(|request| match request.path() {
        "/Items" if request.query("ids").is_some() => items_page(Vec::new(), 0),
        // What 10.11 answers for an unknown `parentId`.
        "/Items" => Reply::status("400 Bad Request").body("Invalid parent id"),
        _ => Reply::status("404 Not Found").body("Playlist not found"),
    });
    let client = session(&url);
    for err in [
        client.album("gone").await.unwrap_err(),
        client.artist("gone").await.unwrap_err(),
        client.playlist("gone").await.unwrap_err(),
        client.collection("gone").await.unwrap_err(),
    ] {
        assert!(matches!(err, AppError::Server { status: 404, .. }), "{err}");
    }

    // The album page drops its cached copy on "returned 404" — however the
    // two requests behind it race.
    let err = join_detail(client.album("gone"), client.album_tracks("gone"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("returned 404"), "{err}");
}

#[tokio::test]
async fn a_detail_page_reports_the_items_error_first() {
    let not_found = || async { Err::<(), _>(not_found("album", "a")) };
    let bad_parent = || async {
        Err::<(), _>(AppError::Server {
            status: 400,
            message: "Invalid parent id".into(),
        })
    };
    let ok = || async { Ok::<_, AppError>(()) };
    let err = join_detail(not_found(), bad_parent()).await.unwrap_err();
    assert!(matches!(err, AppError::Server { status: 404, .. }), "{err}");
    let err = join_detail(ok(), bad_parent()).await.unwrap_err();
    assert!(matches!(err, AppError::Server { status: 400, .. }), "{err}");
    assert!(join_detail(ok(), ok()).await.is_ok());
}

#[test]
fn only_image_data_counts_as_an_image() {
    let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    webp.extend_from_slice(&[0; 8]);
    for image in [
        vec![0xFF, 0xD8, 0xFF, 0xDB, 0, 0],
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(),
        webp,
        b"GIF89a\x01\x00\x01\x00".to_vec(),
        b"\0\0\0\x1cftypavif\0\0\0\0".to_vec(),
    ] {
        assert!(looks_like_image(&image), "{image:?}");
    }
    for other in [
        b"<!DOCTYPE html><html>".to_vec(),
        b"  <html><body>Hotel Wi-Fi".to_vec(),
        b"{\"error\":\"not found\"}".to_vec(),
        b"RIFF\0\0\0\0WAVEfmt ".to_vec(),
        vec![0xFF, 0xD8],
        Vec::new(),
    ] {
        assert!(!looks_like_image(&other), "{other:?}");
    }
}

#[tokio::test]
async fn a_cover_fetch_refuses_a_page_that_answered_200() {
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F'];
    let cover = jpeg.clone();
    let url = serve(move |request| {
        if request.target.contains("/Items/real/") {
            Reply::status("200 OK")
                .header("Content-Type", "image/jpeg")
                .body(cover.clone())
        } else {
            // A captive portal on a plain-http server.
            Reply::status("200 OK")
                .header("Content-Type", "text/html")
                .body("<html><body>Please sign in to the hotel Wi-Fi</body></html>")
        }
    });
    let client = session(&url);
    assert_eq!(client.fetch_image("real", "tag", 300).await.unwrap(), jpeg);
    assert!(client.fetch_image("portal", "tag", 300).await.is_err());
}

#[tokio::test]
async fn a_json_body_is_read_with_a_ceiling() {
    let response = |body: &str| {
        reqwest::Response::from(
            tauri::http::Response::builder()
                .status(200)
                .body(body.as_bytes().to_vec())
                .unwrap(),
        )
    };
    let fits: serde_json::Value = read_json_capped(response("{\"Id\":\"a\"}"), 64)
        .await
        .expect("small body");
    assert_eq!(fits["Id"], "a");

    let err = read_json_capped::<serde_json::Value>(response(&"[1,".repeat(100)), 64)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "server response exceeds the size limit");

    let err = read_json_capped::<ItemsResponse>(response("<html>"), 64)
        .await
        .unwrap_err();
    assert!(
        err.to_string().starts_with("unreadable server response"),
        "{err}"
    );
}

#[tokio::test]
async fn a_sign_out_does_not_wait_out_a_stalled_server() {
    // Accepts the request and never answers in time.
    let url = serve(|_| Reply::status("204 No Content").after(Duration::from_secs(60)));
    let started = Instant::now();
    assert!(session(&url).logout().await.is_err());
    let waited = started.elapsed();
    assert!(
        waited >= LOGOUT_TIMEOUT && waited < LOGOUT_TIMEOUT + Duration::from_secs(5),
        "waited {waited:?}"
    );
}

#[test]
fn a_failed_relogin_answers_the_401s_that_came_before_it() {
    let client = session(&closed_port_url());
    let burst = Instant::now();
    let started = burst + Duration::from_millis(5);
    assert!(client.failed_relogin_since(burst).is_none());

    let err = AppError::Server {
        status: 503,
        message: "Service Unavailable".into(),
    };
    client.record_failed_relogin(started, &err);
    // Every 401 of the burst that started this attempt shares its answer.
    let shared = client.failed_relogin_since(burst).expect("shared failure");
    assert!(matches!(shared, AppError::Server { status: 503, .. }));
    assert_eq!(shared.to_string(), err.to_string());
    // A 401 after the attempt started gets an attempt of its own.
    assert!(client
        .failed_relogin_since(started + Duration::from_millis(1))
        .is_none());
}

#[test]
fn a_replayed_error_reads_the_same() {
    for err in [
        AppError::NotConnected,
        AppError::Server {
            status: 429,
            message: "Too Many Requests".into(),
        },
        AppError::Other("timeout".into()),
        AppError::Audio("device gone".into()),
    ] {
        assert_eq!(err.replay().to_string(), err.to_string());
    }
}

#[test]
fn the_request_stand_in_parses_what_it_needs() {
    let request = Request {
        method: "GET".into(),
        target: "/Items?ids=a&limit=5".into(),
        headers: vec![("user-agent".into(), "x".into())],
    };
    assert_eq!(request.method, "GET");
    assert_eq!(request.path(), "/Items");
    assert_eq!(request.query("limit"), Some("5"));
    assert_eq!(request.query("missing"), None);
    assert_eq!(request.header("user-agent"), Some("x"));
}
