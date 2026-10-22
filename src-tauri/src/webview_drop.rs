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
//! Without it, though, WebView2 takes drops from outside again, and a file or a
//! link dropped where the page does not handle it navigates the window there —
//! the app would be gone from the window. WebView2's own switch for that,
//! `ICoreWebView2Controller4::SetAllowExternalDrop(false)`, cannot be used: with
//! it, a drag *within* the page starts but never reaches a drop target — no
//! `dragenter`, `dragover` or `drop`, only `dragend` (WebView2 155, checked with
//! a real mouse on 22.10.2026) — so every drag of the app would be dead again.
//! Drops from outside therefore stay possible, and what they would do is
//! refused twice. The frontend swallows every drop that no element of the app
//! takes (`src/lib/dropGuard.ts`), so a dropped link or file does not navigate
//! at all — that also covers a URL this side never sees: Tauri lets a
//! navigation through without asking its hooks when the URL does not parse
//! (`http://xn--a.example.com/` is one Chromium accepts and `url` refuses).
//! And here, a webview may only navigate to the app's own pages: Tauri's asset
//! protocol (`http://tauri.localhost` on Windows, `tauri://localhost` on macOS
//! and Linux) and, under `tauri dev`, the dev server. Not "the origin of the
//! page it shows": a window's first page is asked about too, while the webview
//! still shows `about:blank` (checked on 22.10.2026). The same rules cover
//! WebKitGTK and WKWebView, which have no switch at all. A dropped link that
//! asks for a new window is denied by wry already, since no new-window handler
//! is registered.

/// Registers the rule on every webview, including the ones the frontend opens
/// later (mini player, projector).
pub fn init() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("webview-drop")
        .on_navigation(|webview, url| {
            use tauri::Manager;
            let dev = if tauri::is_dev() {
                webview.config().build.dev_url.clone()
            } else {
                None
            };
            navigation_allowed(url, &app_origins(dev))
        })
        .build()
}

/// Where the app's pages come from, as Tauri resolves `WebviewUrl::App`: its
/// asset protocol — on Windows plain http, since `useHttpsScheme` is off — and
/// the dev server when there is one.
fn app_origins(dev: Option<tauri::Url>) -> Vec<tauri::Url> {
    let protocol = if cfg!(windows) || cfg!(target_os = "android") {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    };
    let mut origins = vec![tauri::Url::parse(protocol).expect("a valid URL")];
    origins.extend(dev);
    origins
}

/// Whether `url` lies within one of `origins`: same scheme, host and port.
/// Compared by hand because `url` gives `tauri://` an opaque origin, which
/// equals nothing. A dropped file turns into a `file:` URL and a dropped link
/// into its own address; neither may replace the app.
fn navigation_allowed(url: &tauri::Url, origins: &[tauri::Url]) -> bool {
    url.host_str().is_some()
        && origins.iter().any(|origin| {
            url.scheme() == origin.scheme()
                && url.host_str() == origin.host_str()
                && url.port_or_known_default() == origin.port_or_known_default()
        })
}

#[cfg(test)]
mod tests {
    use super::{app_origins, navigation_allowed};
    use tauri::Url;

    /// Where the app's pages come from: Windows (http, as configured),
    /// macOS/Linux, and `tauri dev`.
    const ORIGINS: [&str; 3] = [
        "http://tauri.localhost/",
        "tauri://localhost/",
        "http://localhost:1420/",
    ];

    fn origins() -> Vec<Url> {
        ORIGINS
            .iter()
            .map(|origin| Url::parse(origin).unwrap())
            .collect()
    }

    fn allowed(url: &str) -> bool {
        navigation_allowed(&Url::parse(url).expect(url), &origins())
    }

    #[test]
    fn a_dropped_file_cannot_replace_the_app() {
        for dropped in [
            "file:///C:/Users/me/Music/Song.flac",
            "FILE:///C:/Users/me/Music/Song.flac",
            "file://nas/music/Song.flac",
            "file://tauri.localhost/Song.flac",
            "file:///home/me/Music/Song.flac",
        ] {
            assert!(!allowed(dropped), "{dropped}");
        }
    }

    #[test]
    fn a_dropped_link_cannot_replace_the_app() {
        for dropped in [
            "https://example.com/",
            "http://example.com/?tauri.localhost",
            "http://tauri.localhost.example.com/",
            "http://tauri.localhost@example.com/",
            "http://tauri.localhost./",
            "https://localhost/",
            "http://localhost:8096/web/index.html",
            "http://127.0.0.1:1420/",
            "http://jfimg.localhost/Items/1/Images/Primary",
            "data:text/html,<p>hi</p>",
            "about:blank",
            "about:srcdoc",
            "ftp://example.com/song.flac",
            "tauri://example.com/",
        ] {
            assert!(!allowed(dropped), "{dropped}");
        }
    }

    #[test]
    fn the_apps_own_pages_still_load() {
        for origin in ORIGINS {
            let base = Url::parse(origin).unwrap();
            for path in ["", "mini", "history/year/2026?from=stats", "projector#top"] {
                let page = base.join(path).unwrap();
                assert!(allowed(page.as_str()), "{page}");
            }
        }
    }

    #[test]
    fn another_scheme_or_port_of_the_same_host_is_another_origin() {
        assert!(!allowed("https://tauri.localhost/"));
        assert!(!allowed("http://tauri.localhost:8080/"));
        assert!(!allowed("tauri://tauri.localhost/"));
        assert!(!allowed("http://localhost:1421/"));
        assert!(!allowed("http://localhost/"));
        assert!(allowed("http://tauri.localhost:80/x"));
    }

    /// `app_origins` takes plain http on Windows; a window with
    /// `useHttpsScheme` would be refused its very first page.
    #[test]
    fn no_window_serves_the_app_over_https() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        for window in config["app"]["windows"].as_array().unwrap() {
            assert_ne!(window["useHttpsScheme"], true, "{window}");
        }
    }

    #[test]
    fn the_dev_server_counts_only_under_tauri_dev() {
        let dev = Url::parse("http://localhost:1420/").unwrap();
        let page = Url::parse("http://localhost:1420/mini").unwrap();
        assert!(navigation_allowed(&page, &app_origins(Some(dev))));
        assert!(!navigation_allowed(&page, &app_origins(None)));
        let app = app_origins(None).remove(0).join("mini").unwrap();
        assert!(navigation_allowed(&app, &app_origins(None)));
    }
}
