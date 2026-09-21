import { flip, type AnimationConfig, type FlipParams } from "svelte/animate";

/**
 * Above this many rows a reorder is not animated. FLIP measures every row on
 * each change and would start one animation per row that moved — removing
 * the first entry of a queue of a few thousand tracks (a played smart view)
 * would animate thousands of rows at once.
 */
export const FLIP_ROW_LIMIT = 400;

/** Svelte animations run through the Web Animations API, which the CSS
 *  reduced-motion rule in app.css does not reach — ask the OS directly. */
function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true
  );
}

/** `animate:` for reorderable lists: a short FLIP slide, unless the list is
 *  too long or the user asked for less motion. */
export function listFlip(
  node: Element,
  rects: { from: DOMRect; to: DOMRect },
  params: FlipParams & { rows: number },
): AnimationConfig {
  const { rows, ...flipParams } = params;
  if (rows > FLIP_ROW_LIMIT || prefersReducedMotion()) return { duration: 0 };
  return flip(node, rects, { duration: 200, ...flipParams });
}

/**
 * Keys for a list whose natural key may be missing or repeat — queue entries
 * persisted by older builds carry an empty `playSessionId`. Duplicate keys
 * make a keyed `{#each}` throw, so a missing or already used id falls back
 * to the row's position.
 */
export function uniqueRowKeys(ids: readonly string[]): string[] {
  const seen = new Set<string>();
  return ids.map((id, index) => {
    const key = id && !seen.has(id) ? id : `row:${index}`;
    seen.add(key);
    return key;
  });
}
