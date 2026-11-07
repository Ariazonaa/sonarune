// Drag & drop reordering of a list, in the list's own positions. A slot is an
// insert-before position: slot `i` lands in front of row `i`, slot `length`
// at the end. Moves are remove-then-insert, which is what the server's
// playlist move and the queue's move do.

/** The slot for a drag over row `index`: in front of it over its upper half,
 *  behind it over the lower. */
export function dropSlot(clientY: number, rect: { top: number; height: number }, index: number): number {
  return clientY < rect.top + rect.height / 2 ? index : index + 1;
}

/** Where the row at `from` ends up when dropped into `slot`, or null when it
 *  stays where it is. Dropping below the origin shifts the target up by one:
 *  the row leaves its place first. */
export function moveTarget(from: number, slot: number): number | null {
  const to = slot > from ? slot - 1 : slot;
  return to === from ? null : to;
}

/** `items` with the one at `from` moved to `to`. */
export function moveItem<T>(items: readonly T[], from: number, to: number): T[] {
  const moved = [...items];
  moved.splice(to, 0, ...moved.splice(from, 1));
  return moved;
}

/** The type a reorder drag carries. Its value is never read: the list keeps
 *  which row is moving itself. */
export const REORDER_MIME = "application/x-sonarune-reorder";

/** Start a reorder drag. It has to carry data: WebKitGTK (Linux) hands GTK
 *  the types in the DataTransfer as the drag's targets, and a drag with none
 *  ends right after `dragstart` -- no `dragover`, no `drop` (seen with the
 *  queue and the playlist on the test VM, 07.11.2026). WebView2 starts one
 *  either way. A type of our own, not text, so dropping a row into another
 *  program does not paste anything. */
export function startReorderDrag(event: DragEvent) {
  const data = event.dataTransfer;
  if (!data) return;
  data.effectAllowed = "move";
  data.setData(REORDER_MIME, "row");
}
