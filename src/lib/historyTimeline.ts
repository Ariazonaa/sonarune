import type { HistoryEntry } from "#lib/types.js";

/** Back-to-back plays of one track, shown as one row. */
export interface TimelineRow {
  /** The newest of the plays; its id keys the row. */
  entry: HistoryEntry;
  /** Every play the row stands for, newest first. */
  entries: HistoryEntry[];
}

/** One local calendar day of the timeline. */
export interface TimelineDay {
  /** `YYYY-MM-DD` in local time. */
  key: string;
  /** Local midnight of the day, for formatting. */
  date: Date;
  rows: TimelineRow[];
}

function dayKey(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

/**
 * Group listens (newest first, as the backend pages them) into local days,
 * and fold a track played several times in a row into one row. Only direct
 * repeats fold: A, B, A stays three rows, because that is what was heard.
 * A repeat across midnight belongs to two days and stays two rows.
 */
export function groupTimeline(entries: HistoryEntry[]): TimelineDay[] {
  const days: TimelineDay[] = [];
  for (const entry of entries) {
    const at = new Date(entry.playedAt);
    const key = dayKey(at);
    let day = days.at(-1);
    if (!day || day.key !== key) {
      day = { key, date: new Date(at.getFullYear(), at.getMonth(), at.getDate()), rows: [] };
      days.push(day);
    }
    const last = day.rows.at(-1);
    if (last && last.entry.track.itemId === entry.track.itemId) {
      last.entries.push(entry);
    } else {
      day.rows.push({ entry, entries: [entry] });
    }
  }
  return days;
}

/** "Today", "Yesterday" or the date, relative to `now`. */
export function dayLabel(
  date: Date,
  now: Date,
  labels: { today: string; yesterday: string },
  locale: string,
): string {
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const days = Math.round((today.getTime() - date.getTime()) / 86_400_000);
  if (days === 0) return labels.today;
  if (days === 1) return labels.yesterday;
  return new Intl.DateTimeFormat(locale, {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: date.getFullYear() === now.getFullYear() ? undefined : "numeric",
  }).format(date);
}

/** Listening time as hours and minutes, e.g. "12 h 05 min" / "42 min". */
export function formatListened(ms: number): { hours: number; minutes: number } {
  const totalMinutes = Math.floor(Math.max(0, ms) / 60_000);
  return { hours: Math.floor(totalMinutes / 60), minutes: totalMinutes % 60 };
}
