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
