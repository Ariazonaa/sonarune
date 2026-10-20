/**
 * The metadata editor's multi-value fields (album artists, genres) are one
 * comma-separated text box each. A comma is a perfectly good character in a
 * name, though — "Earth, Wind & Fire", the genre "Folk, World, & Country" —
 * and Jellyfin does not split on it. A name the item already has therefore
 * stays whole where the text still holds it, comma and all; everything else
 * is split on commas.
 */

/**
 * Split `text` into names: trimmed, empties dropped, deduped case-insensitively
 * (as the backend does), with each of `known` kept whole wherever its pieces
 * still follow each other in the text.
 */
export function splitNames(text: string, known: readonly string[] = []): string[] {
  const parts = text.split(",").map((part) => part.trim());
  // The known names a plain split would cut, longest first, so the one that
  // covers the most of the text wins.
  const whole = known
    .map((name) => ({ name: name.trim(), pieces: name.split(",").map((piece) => piece.trim()) }))
    .filter(({ pieces }) => pieces.length > 1)
    .sort((a, b) => b.pieces.length - a.pieces.length);

  const names: string[] = [];
  const seen = new Set<string>();
  const add = (name: string) => {
    const key = name.toLowerCase();
    if (name && !seen.has(key)) {
      seen.add(key);
      names.push(name);
    }
  };
  for (let index = 0; index < parts.length; ) {
    const match = whole.find(({ pieces }) =>
      pieces.every((piece, offset) => parts[index + offset] === piece),
    );
    if (match) {
      add(match.name);
      index += match.pieces.length;
    } else {
      add(parts[index]);
      index += 1;
    }
  }
  return names;
}

/** Whether two name lists are the same, in the same order. */
export function sameNames(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((name, index) => name === b[index]);
}
