//! Drag and drop into the WebView.
//!
//! Every window runs without Tauri's native drag-and-drop handler
//! (`dragDropEnabled: false` in `tauri.conf.json` and on the windows the
//! frontend opens). On Windows that handler replaces WebView2's own drop
//! target with one that only understands files, so it swallows every HTML5
//! drag — and all of the app's drags are HTML5: queue and playlist rows, tracks
//! and albums onto a playlist in the sidebar. Nothing listens for its file
//! drops either.
//!
//! Without it, though, WebView2 takes drops from outside again, and a file
//! dropped where the page does not handle it opens by navigating the window to
//! that file — the app would be gone from the window. The handler used to rule
//! that out by switching external drops off as well; this does the same on
//! every webview, without the replaced drop target. Drags within the page are
//! not affected by it.
//!
//! WebKitGTK and WKWebView have no such switch, and neither has a WebView2
//! runtime from before 2022. There a dropped file still becomes a navigation
//! to its `file:` URL, so every webview also refuses those: no page of the app
//! is a file.

/// Registers the hooks on every webview, including the ones the frontend
/// opens later (mini player, projector): external drops switched off where
/// the webview can, and no navigation to a dropped file anywhere.
pub fn init() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("webview-drop")
        .on_webview_ready(|webview| refuse_external_drops(&webview))
        .on_navigation(|_, url| navigation_allowed(url))
        .build()
}

/// The app's pages come from Tauri's own protocol (or the dev server); a
/// `file:` URL is what a file dropped onto the window turns into.
fn navigation_allowed(url: &tauri::Url) -> bool {
    url.scheme() != "file"
}

#[cfg(windows)]
fn refuse_external_drops(webview: &tauri::Webview) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller4;
    use windows::core::Interface;

    let result = webview.with_webview(|webview| unsafe {
        // `AllowExternalDrop` needs a WebView2 runtime from 2022 or later; an
        // older one keeps accepting drops, and `navigation_allowed` keeps a
        // dropped file from replacing the app.
        match webview.controller().cast::<ICoreWebView2Controller4>() {
            Ok(controller) => {
                if let Err(e) = controller.SetAllowExternalDrop(false) {
                    tracing::debug!("webview SetAllowExternalDrop(false) failed: {e}");
                }
            }
            Err(e) => tracing::debug!("webview has no ICoreWebView2Controller4: {e}"),
        }
    });
    if let Err(e) = result {
        tracing::debug!("cannot reach the webview to refuse external drops: {e}");
    }
}

#[cfg(not(windows))]
fn refuse_external_drops(_webview: &tauri::Webview) {}

#[cfg(test)]
mod tests {
    use super::navigation_allowed;
    use tauri::Url;

    fn allowed(url: &str) -> bool {
        navigation_allowed(&Url::parse(url).expect(url))
    }

    #[test]
    fn a_dropped_file_cannot_replace_the_app() {
        for dropped in [
            "file:///C:/Users/me/Music/Song.flac",
            "FILE:///C:/Users/me/Music/Song.flac",
            "file://nas/music/Song.flac",
            "file:///home/me/Music/Song.flac",
        ] {
            assert!(!allowed(dropped), "{dropped}");
        }
    }

    #[test]
    fn the_apps_own_pages_still_load() {
        for page in [
            "http://tauri.localhost/",
            "https://tauri.localhost/mini",
            "tauri://localhost/projector",
            "http://localhost:1420/settings",
        ] {
            assert!(allowed(page), "{page}");
        }
    }
}
