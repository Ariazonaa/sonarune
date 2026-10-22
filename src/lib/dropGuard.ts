/**
 * The last word on a drag over a window: a drop that no element of the app
 * took (by calling `preventDefault`) goes nowhere.
 *
 * Left alone, the webview opens a dropped link or file by navigating the
 * window to it. The Rust side refuses navigations away from the app
 * (`webview_drop.rs`), but it never sees a URL it cannot parse — Tauri lets
 * those through — so the drop itself must not navigate. Text fields keep
 * their own handling, so dropped text still lands in them; a file dropped on
 * one is left to the webview, and its `file:` URL to the Rust side.
 *
 * Listens on the window in the bubbling phase, so the app's own drop targets,
 * which accept a drag on the way up, have already had their say.
 */
export function guardDrops(
  target: Window,
  isTextField: (target: EventTarget | null) => boolean,
): () => void {
  const over = (event: DragEvent) => {
    if (event.defaultPrevented || isTextField(event.target)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "none";
  };
  const drop = (event: DragEvent) => {
    if (event.defaultPrevented || isTextField(event.target)) return;
    event.preventDefault();
  };
  target.addEventListener("dragover", over);
  target.addEventListener("drop", drop);
  return () => {
    target.removeEventListener("dragover", over);
    target.removeEventListener("drop", drop);
  };
}
