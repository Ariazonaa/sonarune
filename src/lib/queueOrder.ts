import type { PlayerState, QueueSnapshot, QueueTrack, RepeatMode } from "#lib/types.js";

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

/** Whether the armed stop ends the music after this row (the row the list
 *  marks). By entry id, so of two copies of a track only the armed one
 *  matches; an entry from a build before entry ids goes by its item id. */
export function stopsAfter(
  state: Pick<PlayerState, "stopAfter" | "stopAfterEntry" | "stopAfterItem">,
  track: Pick<QueueTrack, "entryId" | "itemId">,
): boolean {
  if (state.stopAfter === "off") return false;
  if (state.stopAfterEntry && track.entryId) return state.stopAfterEntry === track.entryId;
  return state.stopAfterItem === track.itemId;
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

/** Whether Next has somewhere to go, by the player's own rule (`order_next`
 *  in player/mod.rs): a successor in play order, or the start again under
 *  Repeat All. Repeat One neither holds Next back nor wraps it.
 *
 *  It reads the order as `playOrder` does without building it, and never
 *  touches a track: the always-visible transport asks on every queue change,
 *  and a played smart view runs to thousands of entries. */
export function hasNext(
  queue: Pick<QueueSnapshot, "tracks" | "order" | "index">,
  repeat: RepeatMode,
): boolean {
  const { order, index } = queue;
  const length = queue.tracks.length;
  if (length === 0) return false;
  if (repeat === "all") return true;
  const shuffled = order.length === length && order.every((i) => i >= 0 && i < length);
  // Not in the order: the player finds no position either, and stops.
  const position = shuffled ? order.indexOf(index) : index;
  return position >= 0 && position + 1 < length;
}

/** Item ids in play order: the list the queue panel shows, so what saving the
 *  queue as a playlist keeps — the shuffle order and any rearranging in it. */
export function playOrderItemIds(queue: Pick<QueueSnapshot, "tracks" | "order">): string[] {
  return playOrder(queue).map((entry) => entry.track.itemId);
}
