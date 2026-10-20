import type { Attachment } from "svelte/attachments";
import { confirm } from "#lib/state/confirm.svelte.js";

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

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(", ");

/** The elements Tab reaches inside `root`, in document order. */
export function focusableIn(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (element) => element.tabIndex >= 0 && !element.closest("[hidden], [inert]"),
  );
}

/** Open modals, innermost (most recently opened) last. Only that one keeps Tab. */
const openModals: HTMLElement[] = [];

/**
 * Keyboard focus for a modal dialog. While it is open, keys must not reach the
 * page behind its backdrop — `isModalOpen()` only stops the global shortcuts,
 * not a focused row's own handler or a button's Space/Enter. So:
 *
 * - Opening moves focus into the dialog: to its `[data-autofocus]` element,
 *   else its first control, else its `role="dialog"` element. An overlay that
 *   is mostly reading (track info, the shortcut list) marks the dialog itself.
 * - Tab and Shift+Tab cycle inside it, from wherever focus is.
 * - Closing hands focus back to where it was, if that element is still there
 *   and focus did not move on by itself.
 *
 * Attach it to the overlay's outermost element. On an element that is
 * portaled, declare it after `portal`: attachments run in order, and moving a
 * node blurs whatever was focused inside it.
 */
export const modalFocus: Attachment<HTMLElement> = (node) => {
  const active = document.activeElement;
  const previous = active instanceof HTMLElement && active !== document.body ? active : null;
  openModals.push(node);

  const initial =
    node.querySelector<HTMLElement>("[data-autofocus]") ??
    focusableIn(node)[0] ??
    node.querySelector<HTMLElement>('[role="dialog"]');
  initial?.focus();

  const onKeydown = (event: KeyboardEvent) => {
    if (event.key !== "Tab" || event.defaultPrevented) return;
    if (openModals[openModals.length - 1] !== node) return;
    const controls = focusableIn(node);
    const at = controls.indexOf(document.activeElement as HTMLElement);
    const last = controls.length - 1;
    // From the ends, wrap around; from anywhere that is no control (the
    // dialog itself, or focus lost to <body>), step in. Between controls the
    // browser moves on its own.
    let next: number | null = null;
    if (at === -1) next = event.shiftKey ? last : 0;
    else if (event.shiftKey && at === 0) next = last;
    else if (!event.shiftKey && at === last) next = 0;
    if (next === null) return;
    event.preventDefault();
    controls[next]?.focus();
  };
  window.addEventListener("keydown", onKeydown);

  return () => {
    window.removeEventListener("keydown", onKeydown);
    openModals.splice(openModals.indexOf(node), 1);
    const now = document.activeElement;
    const focusLost = !now || now === document.body || node.contains(now);
    if (focusLost && previous?.isConnected) previous.focus({ preventScroll: true });
  };
};
