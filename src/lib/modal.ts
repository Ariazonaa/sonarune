import { confirm } from "$lib/state/confirm.svelte";

/**
 * Whether a modal dialog is open: the shared confirmation, or any element
 * marked `aria-modal="true"` (every modal overlay sets it; popovers don't).
 * Window-level key handlers of the view underneath bail while one is open —
 * the dialog owns the keyboard, and a dialog opened after the view registered
 * its listener only sees the key after the view's handler has run.
 */
export function isModalOpen(root: ParentNode = document): boolean {
  return confirm.request !== null || root.querySelector('[aria-modal="true"]') !== null;
}
