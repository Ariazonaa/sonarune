import type { QueueSnapshot, QueueTrack } from "$lib/types";

/** A queue entry in play order. `index` is its stored queue position — what
 *  jump and remove take; its position in the list is what `queueMove` takes. */
export interface QueueEntry {
  track: QueueTrack;
  index: number;
}

/** The queue in the order it plays: the shuffle order while shuffle is on,
 *  otherwise as stored. An order that doesn't cover the queue falls back to
 *  the stored order rather than hiding entries. */
export function playOrder(queue: Pick<QueueSnapshot, "tracks" | "order">): QueueEntry[] {
  const { tracks, order } = queue;
  const covers =
    order.length === tracks.length && order.every((index) => index >= 0 && index < tracks.length);
  if (covers) return order.map((index) => ({ track: tracks[index], index }));
  return tracks.map((track, index) => ({ track, index }));
}

/** Play-order position of the current track; -1 for an empty queue. */
export function currentPosition(queue: Pick<QueueSnapshot, "index">, entries: QueueEntry[]): number {
  return entries.findIndex((entry) => entry.index === queue.index);
}

/** The next `count` entries after the current track, in play order. */
export function nextEntries(
  queue: Pick<QueueSnapshot, "tracks" | "order" | "index">,
  count: number,
): QueueEntry[] {
  const entries = playOrder(queue);
  const position = currentPosition(queue, entries);
  return entries.slice(position + 1, position + 1 + count);
}
