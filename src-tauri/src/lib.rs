mod api;
mod audio_devices;
mod cache;
mod commands;
mod commands_discover;
mod commands_library;
mod commands_metadata;
mod commands_visualizer;
mod desktop;
mod diagnostics;
mod download;
mod error;
mod library_watch;
mod player;
mod proto;
mod store;
mod tls;
mod tray;
mod updater;
mod webview_power;

use library_watch::LibraryWatchHandle;
use player::{PlayerHandle, SharedSession};
use std::sync::Arc;
use store::Store;
use tauri::Manager;

pub struct AppState {
    pub store: Arc<Store>,
    pub session: SharedSession,
    pub player: PlayerHandle,
    pub library_watch: LibraryWatchHandle,
    /// Serializes automatic re-login. The app fires many commands at once
    /// (home rows, search, images), so an expired token produces a burst of
    /// 401s; without this every one of them would start its own login.
    pub relogin_gate: Arc<tokio::sync::Mutex<()>>,
    /// Set when a re-login was rejected by the server (as opposed to failing
    /// on the network). Stops the retry loop from hammering `/Users/
    /// AuthenticateByName` with a password the server no longer accepts, which
    /// can trip Jellyfin's brute-force lockout on the user's own account.
    pub relogin_blocked: Arc<std::sync::atomic::AtomicBool>,
}

/// Let the player flush its final "stopped" report and ListenBrainz listen,
/// and cancel in-flight downloads. Every path out of the process goes through
/// this: quitting, and the updater handing over to the installer, which kills
/// us just as abruptly.
pub fn flush_before_exit(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state
            .player
            .shutdown(std::time::Duration::from_millis(2500));
    }
    if let Some(downloads) = app.try_state::<download::DownloadManager>() {
        downloads.cancel_all();
    }
}

/// Shut the app down in order: flush, then let the process (and with it the
/// async runtime) go away.
pub fn quit(app: &tauri::AppHandle) {
    flush_before_exit(app);
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_writer(diagnostics::writer())
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,jellysic_lib=debug".into()),
        )
        .init();

    tauri::Builder::default()
        // A second launch (e.g. while closed to the tray) must not start a
        // second player, tray icon and session on the same device id: it hands
        // over to the running instance, which comes to the front. The plugin
        // has to be registered first.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main_window(app);
        }))
        // In-app updates (updater.rs). Registering the plugin only provides
        // the configured endpoint and public key; the check and the install
        // are our own commands, so no window can reach the plugin's own.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            // Persist size/position/maximized only. Crucially NOT decorations:
            // the window is frameless by config (decorations:false), and a saved
            // state from an older decorated build would otherwise restore the
            // native Windows title bar on top of our custom one.
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .on_window_event(|window, event| {
            // Any window going away releases its visualizer subscription. The
            // frontend's unsubscribe does not survive the WebView teardown,
            // and the IPC channel never reports the dead consumer back, so
            // without this the audio tap keeps producing frames forever.
            if matches!(event, tauri::WindowEvent::Destroyed) {
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.player.tap().unsubscribe_window(window.label());
                }
            }
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let Some(state) = app.try_state::<AppState>() else {
                    return;
                };
                if desktop::load(&state.store).close_behavior == desktop::CloseBehavior::Tray {
                    api.prevent_close();
                    tray::hide_main_window(app);
                } else {
                    quit(app);
                }
            } else if matches!(
                event,
                tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)
            ) {
                tray::sync_main_window_visibility(window.app_handle());
            }
        })
        .register_asynchronous_uri_scheme_protocol("jfimg", |ctx, request, responder| {
            proto::handle(ctx.app_handle().clone(), request, responder);
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let store = Arc::new(Store::open(&data_dir)?);
            let session: SharedSession = Arc::new(tokio::sync::RwLock::new(None));
            let desktop_settings = desktop::load(&store);
            let downloads_dir = app
                .path()
                .download_dir()
                .unwrap_or_else(|_| data_dir.join("downloads"));
            let downloads = download::DownloadManager::new(
                app.handle().clone(),
                session.clone(),
                downloads_dir,
            )?;
            app.manage(downloads);

            // SMTC needs the main window handle; covers for the media overlay
            // come from the jfimg disk cache.
            let hwnd = app
                .get_webview_window("main")
                .and_then(|w| w.hwnd().ok())
                .map(|h| h.0 as isize);
            let cover_dir = app.path().app_cache_dir()?.join("covers");
            let cover_cache =
                cache::CoverCache::new(cover_dir.clone(), desktop_settings.cover_cache_limit_mb)?;
            app.manage(cover_cache.clone());
            cover_cache.schedule_eviction();

            let player = player::spawn(
                app.handle().clone(),
                store.clone(),
                session.clone(),
                hwnd,
                cover_dir,
            );

            // Poll the server for library changes (new music) and tell the UI.
            let library_watch = library_watch::spawn(app.handle().clone(), session.clone());

            app.manage(AppState {
                store,
                session,
                player,
                library_watch,
                relogin_gate: Arc::new(tokio::sync::Mutex::new(())),
                relogin_blocked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });
            tray::setup(app.handle())?;

            if desktop_settings.start_minimized {
                tray::hide_main_window(app.handle());
            }

            if desktop_settings.auto_check_updates {
                updater::check_in_background(app.handle());
            }

            // Bring last session's queue back (paused) so the app never starts
            // with an empty player after a restart.
            let state = app.state::<AppState>();
            if let Some((tracks, index)) = player::load_persisted_queue(&state.store) {
                let _ = state
                    .player
                    .send(player::PlayerCommand::RestoreQueue { tracks, index });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::restore_session,
            commands::connect,
            commands::quick_connect_start,
            commands::quick_connect_poll,
            commands::disconnect,
            commands::list_trusted_certificates,
            commands::forget_trusted_certificate,
            commands::get_albums,
            commands::get_album,
            commands::get_artists,
            commands::get_artist,
            commands::get_songs,
            commands::get_track_info,
            commands_metadata::get_item_metadata,
            commands_metadata::update_item_metadata,
            commands::queue_downloads,
            commands::get_downloads,
            commands::cancel_download,
            commands::retry_download,
            commands::clear_finished_downloads,
            commands::open_downloads_folder,
            commands::play_album,
            commands::enqueue_album,
            commands::enqueue_tracks,
            commands::search,
            commands::search_page,
            commands::player_toggle,
            commands::player_next,
            commands::player_prev,
            commands::player_seek,
            commands::player_set_volume,
            commands::player_set_shuffle_mode,
            commands::player_set_repeat,
            commands::queue_clear,
            commands::queue_remove_played,
            commands::queue_remove_duplicates,
            commands::queue_undo,
            commands::get_player_state,
            commands::get_waveform,
            commands::get_queue,
            commands::get_audio_settings,
            commands::set_audio_settings,
            commands::get_playback_settings,
            commands::set_playback_settings,
            commands::list_audio_devices,
            commands::get_desktop_settings,
            commands::set_desktop_settings,
            commands::set_tray_labels,
            commands::get_cache_info,
            commands::clear_cover_cache,
            commands::export_diagnostics,
            commands::reveal_path,
            commands::play_instant_mix,
            commands::set_sleep_timer,
            commands::player_set_stop_after,
            commands::get_extras_settings,
            commands::set_extras_settings,
            commands::export_settings,
            commands::import_settings,
            commands::visualizer_subscribe,
            commands::visualizer_unsubscribe,
            commands_visualizer::fetch_weekly_preset,
            commands::queue_remove,
            commands::queue_jump,
            commands::queue_move,
            commands::get_lyrics,
            commands_discover::get_genres,
            commands_discover::letter_index,
            commands_discover::get_similar_artists,
            commands_discover::get_discover,
            commands_discover::get_discover_random_albums,
            commands_discover::get_discover_mix_seeds,
            commands_discover::get_discover_similar_artists,
            commands_discover::get_discover_decades,
            commands_discover::get_discover_decade_albums,
            commands_discover::get_long_not_heard_albums,
            commands_discover::play_discover_albums,
            commands_discover::play_discover_tracks,
            commands_discover::play_discover_genre,
            commands_discover::play_discover_decade,
            commands_discover::query_smart_view,
            commands_discover::play_smart_view,
            commands_discover::materialize_smart_view,
            commands_library::get_collections,
            commands_library::get_collection,
            commands_library::play_collection,
            commands_library::enqueue_collection,
            commands_library::get_playlists,
            commands_library::get_playlist,
            commands_library::create_playlist,
            commands_library::duplicate_playlist,
            commands_library::delete_playlist,
            commands_library::rename_playlist,
            commands_library::playlist_add,
            commands_library::playlist_remove,
            commands_library::playlist_move,
            commands_library::transfer_playlist_entries,
            commands_library::play_playlist,
            commands_library::set_favorite,
            commands_library::delete_items,
            commands_library::get_home,
            commands_library::get_stats,
            commands_library::get_favorites,
            commands_library::get_favorite_tracks,
            commands_library::get_favorite_albums,
            commands_library::get_favorite_artists,
            commands::check_library_now,
            commands::check_server,
            updater::check_for_update,
            updater::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod acl_tests {
    //! Per-window command grants (src-tauri/permissions/windows.toml and
    //! src-tauri/capabilities/*.json) must stay consistent with the invoke
    //! handler: build.rs grants the main window every registered command, the
    //! secondary windows get hand-picked sets.

    /// Commands registered in `generate_handler!` in this file.
    fn registered_commands() -> Vec<String> {
        let lib = include_str!("lib.rs");
        let marker = "generate_handler![";
        let start = lib.find(marker).expect("generate_handler!") + marker.len();
        let end = start + lib[start..].find(']').expect("end of generate_handler!");
        lib[start..end]
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.rsplit("::").next().unwrap_or(entry).to_string())
            .collect()
    }

    /// The `permissions` of the `[[set]]` named `identifier` in windows.toml.
    fn set_permissions(identifier: &str) -> Vec<String> {
        let toml = include_str!("../permissions/windows.toml");
        let header = format!("identifier = \"{identifier}\"");
        let start = toml
            .find(&header)
            .unwrap_or_else(|| panic!("set {identifier} in windows.toml"));
        let block = &toml[start..];
        let block = &block[..block.find("[[set]]").unwrap_or(block.len())];
        let list_start = block.find("permissions = [").expect("permissions list");
        let list = &block[list_start..];
        let list = &list[..list.find(']').expect("end of permissions list")];
        list.split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    fn capability(json: &str) -> (Vec<String>, Vec<String>) {
        let value: serde_json::Value = serde_json::from_str(json).expect("capability json");
        let strings = |key: &str| -> Vec<String> {
            value[key]
                .as_array()
                .unwrap_or_else(|| panic!("{key} array"))
                .iter()
                .map(|v| v.as_str().expect("string").to_string())
                .collect()
        };
        (strings("windows"), strings("permissions"))
    }

    #[test]
    fn secondary_window_sets_name_registered_commands() {
        let allowed: std::collections::HashSet<String> = registered_commands()
            .iter()
            .map(|command| format!("allow-{}", command.replace('_', "-")))
            .collect();
        assert!(allowed.len() > 50, "parsed the invoke handler");
        for set in ["mini-window", "projector-window"] {
            let permissions = set_permissions(set);
            assert!(!permissions.is_empty(), "{set} is empty");
            for permission in permissions {
                assert!(
                    allowed.contains(&permission),
                    "{set} grants {permission}, which is not a registered command"
                );
            }
        }
    }

    #[test]
    fn each_window_has_exactly_its_own_capability() {
        let (main_windows, main_permissions) =
            capability(include_str!("../capabilities/main.json"));
        let (mini_windows, mini_permissions) =
            capability(include_str!("../capabilities/mini.json"));
        let (projector_windows, projector_permissions) =
            capability(include_str!("../capabilities/projector.json"));
        assert_eq!(main_windows, ["main"]);
        assert_eq!(mini_windows, ["mini"]);
        assert_eq!(projector_windows, ["projector"]);
        assert!(main_permissions.iter().any(|p| p == "main-window"));
        assert!(mini_permissions.iter().any(|p| p == "mini-window"));
        assert!(projector_permissions
            .iter()
            .any(|p| p == "projector-window"));
        // Only the main window may open further windows.
        for permissions in [&mini_permissions, &projector_permissions] {
            assert!(!permissions.iter().any(|p| p == "main-window"));
            assert!(!permissions
                .iter()
                .any(|p| p == "core:webview:allow-create-webview-window"));
        }
    }
}
