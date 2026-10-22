import type { PlaylistTrackDto } from "#lib/types.js";

// A playlist entry's id is what Jellyfin removes and moves by, but on 10.11 it
// is not one per row: the server hands out the track's own id, so the entries
// of a track that is in the playlist twice share it. Removing by it takes
// every copy, moving by it moves the first one.

/**
 * Keys for the rows of a playlist. The entry id alone cannot be one — a keyed
 * `{#each}` throws on a repeated key, and the playlist would not open at all —
 * so a repeat gets its occurrence appended. That keeps every key stable while
 * the other rows move.
 */
export function entryRowKeys(entries: readonly PlaylistTrackDto[]): string[] {
  const seen = new Map<string, number>();
  return entries.map(({ entryId }) => {
    const occurrence = (seen.get(entryId) ?? 0) + 1;
    seen.set(entryId, occurrence);
    return occurrence === 1 ? `e:${entryId}` : `e:${entryId}#${occurrence}`;
  });
}

/** The entry ids more than one row carries, each with how many do. */
export function sharedEntryIds(entries: readonly PlaylistTrackDto[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const { entryId } of entries) counts.set(entryId, (counts.get(entryId) ?? 0) + 1);
  for (const [entryId, count] of counts) if (count === 1) counts.delete(entryId);
  return counts;
}

/** The playlist with every track's later entries dropped — what cleaning up
 *  duplicates leaves (the first entry of each track stays). */
export function firstOccurrences(entries: readonly PlaylistTrackDto[]): PlaylistTrackDto[] {
  const seen = new Set<string>();
  return entries.filter(({ track }) => {
    if (seen.has(track.id)) return false;
    seen.add(track.id);
    return true;
  });
}

/** A row of the playlist page and its transfer dialog: the entry, where it
 *  stands in the playlist, and its row key (`entryRowKeys`). */
export interface PlaylistRow {
  entry: PlaylistTrackDto;
  originalIndex: number;
  key: string;
}

/**
 * The rows that match a filter, in playlist order — all of them for an empty
 * one. Title, artist and album count, case-insensitively; `haystacks` holds
 * each entry's text lowercased already, so a keystroke in a list of
 * thousands only compares.
 */
export function filterRows(
  entries: readonly PlaylistTrackDto[],
  keys: readonly string[],
  haystacks: readonly string[],
  query: string,
): PlaylistRow[] {
  const needle = query.trim().toLocaleLowerCase();
  const rows: PlaylistRow[] = [];
  entries.forEach((entry, originalIndex) => {
    if (!needle || haystacks[originalIndex].includes(needle)) {
      rows.push({ entry, originalIndex, key: keys[originalIndex] });
    }
  });
  return rows;
}

/** What `filterRows` searches in: title, artist and album, lowercased. A
 *  line break separates them, so a needle cannot match across two. */
export function searchTexts(entries: readonly PlaylistTrackDto[]): string[] {
  return entries.map(({ track }) =>
    [track.name, track.artist, track.album].join("\n").toLocaleLowerCase(),
  );
}
