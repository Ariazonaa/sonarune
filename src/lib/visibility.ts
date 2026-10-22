/**
 * Run `callback` whenever the window becomes visible again. The WebView is
 * suspended while the window is hidden or minimized (src-tauri/src/
 * webview_power.rs), and events emitted meanwhile may never arrive: whatever
 * a store only learns from events is read back then. The player store does
 * the same for its own state. Returns the function that stops listening.
 */
export function onVisible(callback: () => void): () => void {
  if (typeof document === "undefined") return () => {};
  const listener = () => {
    if (document.visibilityState === "visible") callback();
  };
  document.addEventListener("visibilitychange", listener);
  return () => document.removeEventListener("visibilitychange", listener);
}
