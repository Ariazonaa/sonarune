//! Freezing the WebView while the app runs in the background.
//!
//! Audio lives in Rust, not in the WebView (see `player/`). So when the window
//! is hidden to the tray or minimised, the WebView has nothing left to do: it
//! is drawing a window nobody can see, while the music keeps playing from the
//! player thread. WebView2 can be told to release that: `ICoreWebView2_3::
//! TrySuspend` drops the renderer's memory and stops its timers, and `Resume`
//! brings it back.
//!
//! An Electron client cannot do this, because its audio runs inside the
//! renderer it would have to suspend.
//!
//! Suspension is skipped while the visualizer is on screen: it holds a WebGL
//! context and an AudioWorklet, and a projector window may be mirroring it.

#[cfg(windows)]
mod imp {
    use tauri::{Manager, WebviewWindow};
    use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2Controller, ICoreWebView2_3};
    use windows::core::Interface;

    fn with_controller<F>(window: &WebviewWindow, f: F)
    where
        F: FnOnce(ICoreWebView2Controller) + Send + 'static,
    {
        let _ = window.with_webview(move |webview| {
            f(webview.controller().clone());
        });
    }

    /// Ask WebView2 to release the renderer. Best effort: it refuses while a
    /// download, a media stream or a dialog is live, and that refusal is fine.
    ///
    /// The controller has to be marked invisible first. WebView2 rejects
    /// TrySuspend on a visible controller, and minimising the OS window does
    /// not change what the controller thinks.
    pub fn suspend(window: &WebviewWindow) {
        with_controller(window, |controller| unsafe {
            if let Err(e) = controller.SetIsVisible(false) {
                tracing::debug!("webview SetIsVisible(false) failed: {e}");
            }
            let Ok(core) = controller.CoreWebView2() else {
                return;
            };
            let Ok(core3) = core.cast::<ICoreWebView2_3>() else {
                tracing::debug!("webview has no ICoreWebView2_3, cannot suspend");
                return;
            };
            match core3.TrySuspend(None) {
                Ok(()) => tracing::debug!("webview suspend requested"),
                Err(e) => tracing::debug!("webview TrySuspend failed: {e}"),
            }
        });
    }

    /// Bring the renderer back. Safe to call when it was never suspended.
    pub fn resume(window: &WebviewWindow) {
        with_controller(window, |controller| unsafe {
            if let Ok(core) = controller.CoreWebView2() {
                if let Ok(core3) = core.cast::<ICoreWebView2_3>() {
                    match core3.Resume() {
                        Ok(()) => tracing::debug!("webview resumed"),
                        Err(e) => tracing::debug!("webview Resume failed: {e}"),
                    }
                }
            }
            // Visibility last: a visible controller that is still suspended
            // would paint nothing.
            if let Err(e) = controller.SetIsVisible(true) {
                tracing::debug!("webview SetIsVisible(true) failed: {e}");
            }
        });
    }

    /// The main window, if it is still around.
    pub fn main_window(app: &tauri::AppHandle) -> Option<WebviewWindow> {
        app.get_webview_window("main")
    }
}

#[cfg(not(windows))]
mod imp {
    use tauri::{Manager, WebviewWindow};

    pub fn suspend(_window: &WebviewWindow) {}
    pub fn resume(_window: &WebviewWindow) {}
    pub fn main_window(app: &tauri::AppHandle) -> Option<WebviewWindow> {
        app.get_webview_window("main")
    }
}

pub use imp::{main_window, resume, suspend};
