import { m } from "$lib/paraglide/messages";
import type { ShortcutAction } from "$lib/state/shortcuts.svelte";

export function shortcutActionLabel(action: ShortcutAction): string {
  switch (action) {
    case "palette":
      return m.shortcuts_palette();
    case "help":
      return m.shortcuts_help();
    case "playPause":
      return m.cmd_play_pause();
    case "next":
      return m.cmd_next();
    case "previous":
      return m.cmd_prev();
    case "seekForward":
      return m.shortcut_seek_forward();
    case "seekBackward":
      return m.shortcut_seek_backward();
    case "volumeUp":
      return m.shortcut_volume_up();
    case "volumeDown":
      return m.shortcut_volume_down();
    case "mute":
      return m.player_mute();
    case "favorite":
      return m.favorite_toggle();
    case "shuffle":
      return m.player_shuffle();
    case "repeat":
      return m.player_repeat();
    case "jumpToCurrent":
      return m.queue_jump_to_current();
  }
}
