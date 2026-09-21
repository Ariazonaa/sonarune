import { confirm } from "$lib/state/confirm.svelte";
import { palette } from "$lib/state/palette.svelte";
import { player } from "$lib/state/player.svelte";
import { help } from "$lib/state/help.svelte";
import { shortcutSettings, type ShortcutAction } from "$lib/state/shortcuts.svelte";
import { isModalOpen } from "$lib/modal";

function isEditing(): boolean {
  const element = document.activeElement;
  if (!element) return false;
  const tag = element.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    (element as HTMLElement).isContentEditable
  );
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
    if (event.defaultPrevented || isEditing()) return;
    const action = shortcutSettings.actionForEvent(event);
    if (!action || blockedByOverlay(action)) return;
    event.preventDefault();
    runAction(action);
  };

  window.addEventListener("keydown", onKeydown);
  return () => window.removeEventListener("keydown", onKeydown);
}
