import { m } from "#lib/paraglide/messages.js";
import { sonic } from "#lib/state/sonic.svelte.js";
import type { ContextMenuItem } from "#lib/components/ContextMenu.svelte";

/**
 * The sonic path entries of one track's right-click menu (`trackMenu.ts`).
 * While a start is picked, every other track offers to end the path there,
 * to start over from itself instead, or to drop the pick; the start itself
 * offers only the last.
 */
export function sonicPathEntries(track: { id: string; name: string }, icon: string): ContextMenuItem[] {
  const start = sonic.pathStart;
  if (!start) {
    return [
      {
        label: m.ctx_sonic_path_from(),
        icon,
        action: () => sonic.startPath(track.id, track.name),
      },
    ];
  }
  const cancel: ContextMenuItem = {
    label: m.ctx_sonic_path_cancel(),
    icon,
    action: () => sonic.cancelPath(),
  };
  if (start.id === track.id) return [cancel];
  return [
    {
      label: m.ctx_sonic_path_to({ name: start.name }),
      icon,
      action: () => void sonic.finishPath(track.id),
    },
    {
      label: m.ctx_sonic_path_from(),
      icon,
      action: () => sonic.startPath(track.id, track.name),
    },
    cancel,
  ];
}
