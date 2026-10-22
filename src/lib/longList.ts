/**
 * Above this many entries a list renders only the rows in view (virtua), as
 * /songs does: a materialized smart view runs to thousands of tracks, and
 * building a row with its cover for each takes seconds — on opening, and
 * again on every filter keystroke that touches them all. Up to it the plain
 * list stays, whose reorders animate (`listFlip`).
 *
 * Used by the playlist page and its transfer dialog; the queue panel has its
 * own limit (`FLIP_ROW_LIMIT`).
 */
export const LONG_LIST_ROWS = 300;

export function isLongList(count: number): boolean {
  return count > LONG_LIST_ROWS;
}

/** The element `node` scrolls in: its nearest ancestor that scrolls
 *  vertically (on a page, the shell's `<main>`). */
export function scrollParent(node: HTMLElement): HTMLElement | null {
  for (let element = node.parentElement; element; element = element.parentElement) {
    const { overflowY } = getComputedStyle(element);
    if (overflowY === "auto" || overflowY === "scroll") return element;
  }
  return null;
}

/** How far below the top of `scroller`'s content `node` starts — what lies
 *  above a list in the same scroller (a page's header, a padding). */
export function offsetIn(node: HTMLElement, scroller: HTMLElement): number {
  return (
    node.getBoundingClientRect().top -
    scroller.getBoundingClientRect().top -
    scroller.clientTop +
    scroller.scrollTop
  );
}

/** Where a list was scrolled to, kept for the way back (SvelteKit
 *  `snapshot`). */
export interface ListPosition {
  /** The scroller's offset. */
  top: number;
  /** The row at the top of the view and how far into it, where known (-1:
   *  above the first row, or not measured). A long list goes back by these,
   *  since it has not measured the rows above when it opens again. */
  index: number;
  within: number;
}
