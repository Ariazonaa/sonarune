//! `jfimg://` protocol: authenticated, disk-cached album art for the WebView.
//! The frontend loads `http://jfimg.localhost/{itemId}/{tag}/{size}` (Windows
//! WebView2 URL form); we fetch from Jellyfin with the auth header and cache
//! the bytes on disk. Keeps tokens out of the DOM and works with self-signed
//! certificate servers.

use crate::cache::CoverCache;
use crate::AppState;
use std::path::{Path, PathBuf};
use tauri::http::{Response, StatusCode};
use tauri::Manager;

/// Bump the file's mtime so eviction treats it as recently used (Windows
/// atime is unreliable).
fn touch(path: &Path) {
    let _ = std::fs::File::options()
        .append(true)
        .open(path)
        .and_then(|f| {
            f.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
        });
}

fn cache_path(app: &tauri::AppHandle, item_id: &str, tag: &str, size: u32) -> Option<PathBuf> {
    let cache = app.try_state::<CoverCache>()?;
    // Both components originate from the server. The URL split above already
    // rejects extra path segments, but validate anyway: this is the same name
    // the player builds from raw JSON, and the rule belongs in one place.
    Some(
        cache
            .dir()
            .join(crate::cache::cover_file_name(item_id, tag, size)?),
    )
}

/// A cached cover, or `None` on a miss. A zero-length file is a leftover from
/// a crashed/partial write — ignore it and re-fetch rather than serving a
/// broken image.
fn read_cached(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok().filter(|bytes| !bytes.is_empty())?;
    touch(path);
    Some(bytes)
}

fn not_found() -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        // covers are drawn onto a canvas (ambient colors); without ACAO the
        // cross-origin jfimg response would taint it
        .header("Access-Control-Allow-Origin", "*")
        .body(Vec::new())
        .unwrap()
}

fn ok_image(bytes: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "image/jpeg")
        // tag in the URL changes when the image changes -> cache forever
        .header("Cache-Control", "public, max-age=31536000, immutable")
        .header("Access-Control-Allow-Origin", "*")
        .body(bytes)
        .unwrap()
}

pub fn handle(
    app: tauri::AppHandle,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    // Path: /{itemId}/{tag}/{size}
    let path = request.uri().path().to_string();
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let [item_id, tag, size] = parts.as_slice() else {
        responder.respond(not_found());
        return;
    };
    let (item_id, tag) = (item_id.to_string(), tag.to_string());
    let Ok(size) = size.parse::<u32>() else {
        responder.respond(not_found());
        return;
    };

    // No disk I/O on this thread: the webview invokes even the asynchronous
    // protocol handler on the UI thread, and a scrolling grid asks for dozens
    // of covers at once.
    tauri::async_runtime::spawn(async move {
        let path = cache_path(&app, &item_id, &tag, size);
        if let Some(path) = path.clone() {
            let cached = tauri::async_runtime::spawn_blocking(move || read_cached(&path))
                .await
                .ok()
                .flatten();
            if let Some(bytes) = cached {
                responder.respond(ok_image(bytes));
                return;
            }
        }

        let state = app.state::<AppState>();
        // Clone out of the lock before awaiting the network. Holding the read
        // guard across `fetch_image` (30 s timeout, dozens in flight while a
        // grid scrolls) blocks any writer, and tokio's RwLock is write-
        // preferring — a waiting writer then stalls the player thread's
        // `blocking_read`, freezing playback controls entirely.
        let client = { state.session.read().await.clone() };
        let Some(client) = client else {
            responder.respond(not_found());
            return;
        };
        let bytes = match client.fetch_image(&item_id, &tag, size).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!("cover fetch failed for {item_id}: {e}");
                responder.respond(not_found());
                return;
            }
        };
        let Some(path) = path else {
            responder.respond(ok_image(bytes));
            return;
        };
        // Crash-safe temp-file-and-rename write, so a reader never sees a
        // partial file. The bytes travel through the blocking task and back
        // instead of being copied for it.
        let written = tauri::async_runtime::spawn_blocking(move || {
            let ok = crate::cache::write_file_atomic(&path, &bytes).is_ok();
            (bytes, ok)
        })
        .await;
        match written {
            Ok((bytes, ok)) => {
                if ok {
                    if let Some(cache) = app.try_state::<CoverCache>() {
                        cache.schedule_eviction();
                    }
                }
                responder.respond(ok_image(bytes));
            }
            Err(e) => {
                tracing::debug!("cover cache write task failed for {item_id}: {e}");
                responder.respond(not_found());
            }
        }
    });
}
