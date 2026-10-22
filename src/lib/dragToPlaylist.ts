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
export const DRAG_MIME = "application/x-sonarune-add";

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

/**
 * Whether a drag that started in this window is under way. The MIME type alone
 * proves nothing: a page in another Chromium-based app can set it on its own
 * drag and drop it onto a playlist here.
 */
let started = false;

/** Start a drag carrying `payload`. */
export function startDrag(event: DragEvent, payload: DragPayload) {
  const data = event.dataTransfer;
  if (!data) return;
  data.effectAllowed = "copy";
  data.setData(DRAG_MIME, encodeDrag(payload));
  data.setData("text/plain", (payload.trackIds ?? [payload.albumId ?? ""]).join("\n"));
  started = true;
  // The end of the drag, wherever it happens: `dragend` on the source — on the
  // element itself too, since a virtual list may have dropped it from the page
  // meanwhile and the event then never passes the window — or a drop here once
  // the drop targets have read the payload.
  const source = event.target;
  const end = () => {
    started = false;
    source?.removeEventListener("dragend", end);
    window.removeEventListener("dragend", end, true);
    window.removeEventListener("drop", end);
  };
  source?.addEventListener("dragend", end);
  window.addEventListener("dragend", end, true);
  window.addEventListener("drop", end);
}

/** The payload of a drag in progress, or null when it is not one of ours. */
export function readDrag(event: DragEvent): DragPayload | null {
  if (!started) return null;
  return decodeDrag(event.dataTransfer?.getData(DRAG_MIME));
}

/**
 * Whether a drag in progress is ours, judged during `dragover` — where the
 * data itself is not readable, only the list of types it carries.
 */
export function isOurDrag(event: DragEvent): boolean {
  return started && (event.dataTransfer?.types.includes(DRAG_MIME) ?? false);
}
