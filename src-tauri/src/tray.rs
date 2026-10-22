use crate::player::{PlaybackStatus, PlayerCommand, PlayerState};
use crate::AppState;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::menu::{IsMenuItem, Menu, MenuItem, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const MENU_WINDOW: &str = "tray-window";
const MENU_PLAY_PAUSE: &str = "tray-play-pause";
const MENU_PREVIOUS: &str = "tray-previous";
const MENU_NEXT: &str = "tray-next";
const MENU_QUIT: &str = "tray-quit";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    pub show: String,
    pub hide: String,
    pub play: String,
    pub pause: String,
    pub previous: String,
    pub next: String,
    pub quit: String,
    pub nothing_playing: String,
}

impl Default for TrayLabels {
    fn default() -> Self {
        // Neutral symbols are visible only for the brief period before the
        // frontend supplies localized labels from Paraglide.
        Self {
            show: "Sonarune".into(),
            hide: "Sonarune".into(),
            play: "▶".into(),
            pause: "⏸".into(),
            previous: "⏮".into(),
            next: "⏭".into(),
            quit: "⏻".into(),
            nothing_playing: "Sonarune".into(),
        }
    }
}

pub struct TrayState {
    _tray: TrayIcon<tauri::Wry>,
    window: MenuItem<tauri::Wry>,
    now_playing: MenuItem<tauri::Wry>,
    play_pause: MenuItem<tauri::Wry>,
    previous: MenuItem<tauri::Wry>,
    next: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
    labels: Mutex<TrayLabels>,
    last_player: Mutex<Option<(String, bool, usize)>>,
    last_window_visible: Mutex<Option<bool>>,
    /// Set by a double-click, so the button release that completes it does
    /// not toggle the window again (see [`left_click`]).
    ignore_next_release: AtomicBool,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let labels = TrayLabels::default();
    let window = MenuItemBuilder::with_id(MENU_WINDOW, &labels.show).build(app)?;
    let now_playing = MenuItemBuilder::new(&labels.nothing_playing)
        .enabled(false)
        .build(app)?;
    let play_pause = MenuItemBuilder::with_id(MENU_PLAY_PAUSE, &labels.play).build(app)?;
    let previous = MenuItemBuilder::with_id(MENU_PREVIOUS, &labels.previous).build(app)?;
    let next = MenuItemBuilder::with_id(MENU_NEXT, &labels.next).build(app)?;
    let quit = MenuItemBuilder::with_id(MENU_QUIT, &labels.quit).build(app)?;
    let separator_a = PredefinedMenuItem::separator(app)?;
    let separator_b = PredefinedMenuItem::separator(app)?;
    let items: [&dyn IsMenuItem<_>; 8] = [
        &window,
        &now_playing,
        &separator_a,
        &previous,
        &play_pause,
        &next,
        &separator_b,
        &quit,
    ];
    let menu = Menu::with_items(app, &items)?;

    let mut builder = TrayIconBuilder::with_id("sonarune-tray")
        .menu(&menu)
        .tooltip("Sonarune")
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(handle_tray_event);
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    let tray = builder.build(app)?;

    app.manage(TrayState {
        _tray: tray,
        window,
        now_playing,
        play_pause,
        previous,
        next,
        quit,
        labels: Mutex::new(labels),
        last_player: Mutex::new(None),
        last_window_visible: Mutex::new(None),
        ignore_next_release: AtomicBool::new(false),
    });
    refresh(app);
    Ok(())
}

/// Take the icon out of the notification area, or put it back.
///
/// The icon goes away by itself only when its last handle is dropped, and
/// `TrayState` holds one until the end — every exit ends in
/// `std::process::exit` (tao's after its loop, the updater's after launching
/// the installer), which runs no destructors. Without this a dead icon stays
/// until the pointer passes over it, next to the new one after an update.
pub fn set_icon_visible(app: &AppHandle, visible: bool) {
    if let Some(tray) = app.try_state::<TrayState>() {
        let _ = tray._tray.set_visible(visible);
    }
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id() {
        id if id == MENU_WINDOW => toggle_main_window(app),
        id if id == MENU_PLAY_PAUSE => send(app, PlayerCommand::Toggle),
        id if id == MENU_PREVIOUS => send(app, PlayerCommand::Prev),
        id if id == MENU_NEXT => send(app, PlayerCommand::Next),
        id if id == MENU_QUIT => crate::quit(app),
        _ => {}
    }
}

fn handle_tray_event(tray: &TrayIcon<tauri::Wry>, event: TrayIconEvent) {
    let button = match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Down,
            ..
        } => LeftButton::Press,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => LeftButton::Release,
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => LeftButton::DoubleClick,
        _ => return,
    };
    let app = tray.app_handle();
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    match left_click(&state.ignore_next_release, button) {
        ClickAction::Toggle => toggle_main_window(app),
        ClickAction::Show => show_main_window(app),
        ClickAction::Nothing => {}
    }
}

#[derive(Debug, Clone, Copy)]
enum LeftButton {
    Press,
    Release,
    DoubleClick,
}

#[derive(Debug, PartialEq, Eq)]
enum ClickAction {
    Nothing,
    Toggle,
    Show,
}

/// What a left button event on the icon does.
///
/// A click toggles the window on release. Windows reports a double-click as
/// press, release, double-click, release, so toggling on every release showed
/// the window and hid it again at once. A double-click shows the window — what
/// it means everywhere on Windows — and the release that completes it is
/// ignored. The next press forgets that, in case the release happened off the
/// icon and never arrived.
fn left_click(ignore_next_release: &AtomicBool, button: LeftButton) -> ClickAction {
    match button {
        LeftButton::Press => {
            ignore_next_release.store(false, Ordering::Relaxed);
            ClickAction::Nothing
        }
        LeftButton::Release if ignore_next_release.swap(false, Ordering::Relaxed) => {
            ClickAction::Nothing
        }
        LeftButton::Release => ClickAction::Toggle,
        LeftButton::DoubleClick => {
            ignore_next_release.store(true, Ordering::Relaxed);
            ClickAction::Show
        }
    }
}

fn send(app: &AppHandle, command: PlayerCommand) {
    if let Some(state) = app.try_state::<AppState>() {
        let _ = state.player.send(command);
    }
}

pub fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false);
    if visible {
        let _ = window.hide();
        update_window_visibility(app, false);
    } else {
        show_main_window(app);
    }
}

/// Bring the main window forward (from the tray, minimized or hidden).
pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    update_window_visibility(app, true);
}

pub fn hide_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    update_window_visibility(app, false);
}

pub fn sync_main_window_visibility(app: &AppHandle) {
    let visible = app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false)
    });
    update_window_visibility(app, visible);
}

pub fn set_labels(app: &AppHandle, labels: TrayLabels) {
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    *tray.labels.lock().unwrap() = labels;
    *tray.last_player.lock().unwrap() = None;
    *tray.last_window_visible.lock().unwrap() = None;
    refresh(app);
}

pub fn update_player(app: &AppHandle, state: &PlayerState) {
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    let labels = tray.labels.lock().unwrap().clone();
    let current = state.current.as_ref().map(|track| {
        let value = if track.artist.is_empty() {
            track.name.clone()
        } else {
            format!("{} – {}", track.artist, track.name)
        };
        truncate(&value, 80)
    });
    let now_playing = current.unwrap_or_else(|| labels.nothing_playing.clone());
    // A load that arrives paused offers Play, like the window's transport.
    let playing = state.status == PlaybackStatus::Playing
        || (state.status == PlaybackStatus::Loading && !state.pending_pause);
    let key = (now_playing.clone(), playing, state.queue_len);
    let mut last = tray.last_player.lock().unwrap();
    if last.as_ref() == Some(&key) {
        return;
    }
    *last = Some(key);
    drop(last);

    let _ = tray.now_playing.set_text(menu_text(&now_playing));
    let _ = tray
        .play_pause
        .set_text(if playing { &labels.pause } else { &labels.play });
    let has_queue = state.queue_len > 0;
    let _ = tray.play_pause.set_enabled(has_queue);
    let _ = tray.previous.set_enabled(has_queue);
    let _ = tray.next.set_enabled(has_queue);
    let tooltip = if state.current.is_some() {
        format!("Sonarune — {now_playing}")
    } else {
        "Sonarune".to_string()
    };
    let _ = tray._tray.set_tooltip(Some(tooltip));
}

fn update_window_visibility(app: &AppHandle, visible: bool) {
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    let mut last = tray.last_window_visible.lock().unwrap();
    if *last == Some(visible) {
        return;
    }
    *last = Some(visible);
    drop(last);
    // Nothing to draw while hidden, and the music comes from the player
    // thread, not the WebView. Let WebView2 release the renderer.
    //
    // Except while a visualizer runs in it: it holds a WebGL context and an
    // AudioWorklet fed over an IPC channel, and suspending tears both down
    // under it. The spectrum bar on Now Playing does not count: its frames
    // are held back while the page sleeps (a frozen page takes none) and
    // flow again once it is resumed.
    if let Some(window) = crate::webview_power::main_window(app) {
        let tap = app
            .try_state::<AppState>()
            .map(|state| state.player.tap().clone());
        if visible {
            crate::webview_power::resume(&window);
            if let Some(tap) = &tap {
                tap.set_window_paused("main", false);
            }
        } else {
            let visualizing = tap
                .as_ref()
                .is_some_and(|tap| tap.keeps_window_awake("main"));
            if !visualizing {
                if let Some(tap) = &tap {
                    tap.set_window_paused("main", true);
                }
                crate::webview_power::suspend(&window);
            }
        }
    }
    let labels = tray.labels.lock().unwrap();
    let _ = tray
        .window
        .set_text(if visible { &labels.hide } else { &labels.show });
    let _ = tray.quit.set_text(&labels.quit);
    let _ = tray.previous.set_text(&labels.previous);
    let _ = tray.next.set_text(&labels.next);
}

fn refresh(app: &AppHandle) {
    sync_main_window_visibility(app);
    if let Some(state) = app.try_state::<AppState>() {
        update_player(app, &state.player.state());
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut shortened: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

/// Menu item text shows `&` as written. The menu treats a single `&` as the
/// marker of the next character's access key, so "Simon & Garfunkel" would
/// read "Simon  Garfunkel"; `&&` is a literal one. Tooltips have no access
/// keys and take the text as it is.
fn menu_text(text: &str) -> String {
    text.replace('&', "&&")
}

#[cfg(test)]
mod tests {
    use super::{left_click, menu_text, truncate, ClickAction, LeftButton};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn tray_title_truncates_on_character_boundaries() {
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("äöüß", 3), "äö…");
    }

    #[test]
    fn ampersands_in_menu_text_stay_visible() {
        assert_eq!(
            menu_text("Simon & Garfunkel – The Boxer"),
            "Simon && Garfunkel – The Boxer"
        );
        assert_eq!(menu_text("R&B && more"), "R&&B &&&& more");
        assert_eq!(menu_text("Nothing to escape"), "Nothing to escape");
    }

    fn clicks(buttons: &[LeftButton]) -> Vec<ClickAction> {
        let ignore_next_release = AtomicBool::new(false);
        buttons
            .iter()
            .map(|&button| left_click(&ignore_next_release, button))
            .filter(|action| *action != ClickAction::Nothing)
            .collect()
    }

    #[test]
    fn a_click_toggles_the_window() {
        use LeftButton::*;
        assert_eq!(clicks(&[Press, Release]), [ClickAction::Toggle]);
        assert_eq!(
            clicks(&[Press, Release, Press, Release]),
            [ClickAction::Toggle, ClickAction::Toggle]
        );
    }

    #[test]
    fn a_double_click_shows_the_window_and_does_not_toggle_it_back() {
        use LeftButton::*;
        // What Windows delivers: down, up, double-click, up. The first release
        // toggles (shows a hidden window), the double-click makes sure it is
        // shown, and the last release must not hide it again.
        assert_eq!(
            clicks(&[Press, Release, DoubleClick, Release]),
            [ClickAction::Toggle, ClickAction::Show]
        );
        // A click after that works normally again.
        assert_eq!(
            clicks(&[Press, Release, DoubleClick, Release, Press, Release]),
            [ClickAction::Toggle, ClickAction::Show, ClickAction::Toggle]
        );
    }

    #[test]
    fn a_double_click_released_off_the_icon_does_not_swallow_the_next_click() {
        use LeftButton::*;
        assert_eq!(
            clicks(&[Press, Release, DoubleClick, Press, Release]),
            [ClickAction::Toggle, ClickAction::Show, ClickAction::Toggle]
        );
    }
}
