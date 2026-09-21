import { api } from "$lib/api";
import { m } from "$lib/paraglide/messages";

/** Keep the native tray menu localized with the same Paraglide messages as
 * the WebView. Safe to call repeatedly; Rust only updates changed labels. */
export function syncTrayLabels(): Promise<void> {
  return api.setTrayLabels({
    show: m.tray_show(),
    hide: m.tray_hide(),
    play: m.player_play(),
    pause: m.player_pause(),
    previous: m.player_previous(),
    next: m.player_next(),
    quit: m.tray_quit(),
    nothingPlaying: m.player_nothing_playing(),
  });
}
