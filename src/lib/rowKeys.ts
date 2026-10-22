/**
 * Whether a keydown on a clickable list row — a focusable `role="button"`
 * row whose click plays its track — activates the row the way a click does.
 *
 * Only a plain Enter pressed on the row itself does:
 * - A key from a link or button inside the row belongs to that control.
 *   Cancelling it at the row would swallow the control's own activation, so
 *   Enter on an artist link would play the album instead of opening the
 *   artist.
 * - Space stays with the global play/pause shortcut. A click leaves its row
 *   focused, and a row that took Space would restart its track on every
 *   press instead of pausing it.
 * - Enter with a modifier may be bound to a shortcut, and a held-down Enter
 *   would start the same row over and over.
 */
export function activatesRow(event: KeyboardEvent): boolean {
  return (
    event.key === "Enter" &&
    event.target === event.currentTarget &&
    !event.repeat &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !event.shiftKey
  );
}
