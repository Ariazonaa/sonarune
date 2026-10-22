import { confirm } from "#lib/state/confirm.svelte.js";
import { palette } from "#lib/state/palette.svelte.js";
import { player } from "#lib/state/player.svelte.js";
import { help } from "#lib/state/help.svelte.js";
import { shortcutSettings, type ShortcutAction } from "#lib/state/shortcuts.svelte.js";
import { controlOwnsKey } from "#lib/controls.js";
import { isModalOpen } from "#lib/modal.js";

/** Actions a held key may repeat. A toggle repeated at the keyboard's ~30/s
 *  would flap (play/pause, mute, shuffle) or fire a burst of opposite
 *  favorite writes the server applies in whatever order they arrive. */
const REPEATABLE = new Set<ShortcutAction>(["seekForward", "seekBackward", "volumeUp", "volumeDown"]);

/**
 * The palette's Ctrl/⌘ chord. It means nothing to a text field, and Ctrl+P
 * left to the WebView opens its print dialog over the app. Not with Alt held:
 * Ctrl+Alt is AltGr, which types characters.
 */
function isPaletteChord(event: KeyboardEvent, action: ShortcutAction): boolean {
  return action === "palette" && (event.ctrlKey || event.metaKey) && !event.altKey;
}

/**
 * Whether the focused control keeps `event` (see `controlOwnsKey`). The
 * palette chord is the exception, so it works while typing. That is also how
 * the palette, whose own field has focus, closes with the key that opened it.
 */
function focusKeepsKey(event: KeyboardEvent, action: ShortcutAction): boolean {
  return controlOwnsKey(document.activeElement, event) && !isPaletteChord(event, action);
}

/** Whether an open overlay swallows `action`. The palette and the shortcut
 *  help keep their own toggle, so the key that opened one closes it; any
 *  other modal (a confirmation, the metadata editor, …) owns the keyboard
 *  while it is open. */
export function blockedByOverlay(action: ShortcutAction): boolean {
  if (confirm.request) return true;
  if (palette.open) return action !== "palette";
  if (help.open) return action !== "help";
  return isModalOpen();
}

function runAction(action: ShortcutAction) {
  switch (action) {
    case "palette":
      palette.toggle();
      return;
    case "help":
      help.toggle();
      return;
    case "playPause":
      player.run(player.toggle());
      return;
    case "next":
      player.run(player.next());
      return;
    case "previous":
      player.run(player.prev());
      return;
    case "seekForward":
      player.run(player.seekBy(5000));
      return;
    case "seekBackward":
      player.run(player.seekBy(-5000));
      return;
    case "volumeUp":
      player.run(player.nudgeVolume(0.05));
      return;
    case "volumeDown":
      player.run(player.nudgeVolume(-0.05));
      return;
    case "mute":
      player.run(player.toggleMute());
      return;
    case "favorite":
      player.toggleCurrentFavorite();
      return;
    case "shuffle":
      player.run(player.toggleShuffle());
      return;
    case "repeat":
      player.run(player.cycleRepeat());
      return;
    case "jumpToCurrent":
      player.jumpToCurrent();
  }
}

export function installShortcuts(): () => void {
  const onKeydown = (event: KeyboardEvent) => {
    if (event.defaultPrevented) return;
    const action = shortcutSettings.actionForEvent(event);
    if (!action || focusKeepsKey(event, action)) return;
    if (blockedByOverlay(action)) {
      // A dialog that keeps the palette shut (a confirmation, the metadata
      // editor) still must not let its chord through to the WebView: Ctrl+P
      // would open the print dialog over it. A plain key bound to the palette
      // stays with the dialog's own controls (Space on its button).
      if (isPaletteChord(event, action)) event.preventDefault();
      return;
    }
    // Repeats are swallowed too, not handed back: a later repeat that is not
    // prevented would still activate a focused button on key-up.
    event.preventDefault();
    if (event.repeat && !REPEATABLE.has(action)) return;
    runAction(action);
  };

  window.addEventListener("keydown", onKeydown);
  return () => window.removeEventListener("keydown", onKeydown);
}
