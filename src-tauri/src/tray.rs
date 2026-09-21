use crate::player::{PlaybackStatus, PlayerCommand, PlayerState};
use crate::AppState;
use serde::Deserialize;
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
            show: "Jellysic".into(),
            hide: "Jellysic".into(),
            play: "▶".into(),
            pause: "⏸".into(),
            previous: "⏮".into(),
            next: "⏭".into(),
            quit: "⏻".into(),
            nothing_playing: "Jellysic".into(),
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

    let mut builder = TrayIconBuilder::with_id("jellysic-tray")
        .menu(&menu)
        .tooltip("Jellysic")
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
    });
    refresh(app);
    Ok(())
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
    if matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
    ) {
        toggle_main_window(tray.app_handle());
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
    let playing = matches!(
        state.status,
        PlaybackStatus::Playing | PlaybackStatus::Loading
    );
    let key = (now_playing.clone(), playing, state.queue_len);
    let mut last = tray.last_player.lock().unwrap();
    if last.as_ref() == Some(&key) {
        return;
    }
    *last = Some(key);
    drop(last);

    let _ = tray.now_playing.set_text(&now_playing);
    let _ = tray
        .play_pause
        .set_text(if playing { &labels.pause } else { &labels.play });
    let has_queue = state.queue_len > 0;
    let _ = tray.play_pause.set_enabled(has_queue);
    let _ = tray.previous.set_enabled(has_queue);
    let _ = tray.next.set_enabled(has_queue);
    let tooltip = if state.current.is_some() {
        format!("Jellysic — {now_playing}")
    } else {
        "Jellysic".to_string()
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
    // Except while the visualizer is running: it holds a WebGL context and an
    // AudioWorklet fed over an IPC channel, and suspending tears both down
    // under it. Someone who sends the window to the tray with the visualizer
    // open (onto a second monitor, say) wants it to keep drawing.
    if let Some(window) = crate::webview_power::main_window(app) {
        if visible {
            crate::webview_power::resume(&window);
        } else {
            let visualizing = app
                .try_state::<AppState>()
                .is_some_and(|state| state.player.tap().has_subscribers_in("main"));
            if !visualizing {
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

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn tray_title_truncates_on_character_boundaries() {
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("äöüß", 3), "äö…");
    }
}
