/**
 * Dragging tracks and albums onto a playlist in the sidebar.
 *
 * The payload travels as a custom MIME type so nothing else on the page
 * mistakes it for a link or a file, and as text/plain too — a drag that leaves
 * the window then drops a readable list of ids instead of nothing.
 *
 * An album travels as its id rather than its tracks: the grid card does not
 * have them, and asking the server once at drop time is cheaper than asking
 * for every album someone drags across the window.
 */
export const DRAG_MIME = "application/x-jellysic-add";

export interface DragPayload {
  trackIds?: string[];
  albumId?: string;
  /** For the drop feedback: what the user is dragging. */
  label?: string;
}

export function encodeDrag(payload: DragPayload): string {
  return JSON.stringify(payload);
}

/** Parse a payload; null for anything that is not one of ours. */
export function decodeDrag(text: string | null | undefined): DragPayload | null {
  if (!text) return null;
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!value || typeof value !== "object") return null;
  const input = value as Partial<DragPayload>;
  const trackIds = Array.isArray(input.trackIds)
    ? input.trackIds.filter((id): id is string => typeof id === "string")
    : undefined;
  const albumId = typeof input.albumId === "string" ? input.albumId : undefined;
  if ((!trackIds || trackIds.length === 0) && !albumId) return null;
  return {
    ...(trackIds && trackIds.length > 0 ? { trackIds } : {}),
    ...(albumId ? { albumId } : {}),
    ...(typeof input.label === "string" ? { label: input.label } : {}),
  };
}

/** Start a drag carrying `payload`. */
export function startDrag(event: DragEvent, payload: DragPayload) {
  const data = event.dataTransfer;
  if (!data) return;
  data.effectAllowed = "copy";
  data.setData(DRAG_MIME, encodeDrag(payload));
  data.setData("text/plain", (payload.trackIds ?? [payload.albumId ?? ""]).join("\n"));
}

/** The payload of a drag in progress, or null when it is not one of ours. */
export function readDrag(event: DragEvent): DragPayload | null {
  return decodeDrag(event.dataTransfer?.getData(DRAG_MIME));
}

/**
 * Whether a drag in progress is ours, judged during `dragover` — where the
 * data itself is not readable, only the list of types it carries.
 */
export function isOurDrag(event: DragEvent): boolean {
  return event.dataTransfer?.types.includes(DRAG_MIME) ?? false;
}
