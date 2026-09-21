import type { Attachment } from "svelte/attachments";

/**
 * Keyboard support for menus, after the WAI-ARIA menu pattern.
 *
 * `Popover.svelte` with `role="menu"` (its default) runs the keys inside an
 * open menu — see `onMenuKeydown` there. Items are the elements with a
 * menuitem role; disabled ones carry `aria-disabled="true"` and are skipped.
 * This module holds the pieces menus, rows and menu triggers share.
 */

/** The elements a menu's arrow keys move between. */
export const MENU_ITEM = '[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]';

const MODIFIERS = new Set(["Control", "Shift", "Alt", "Meta", "AltGraph"]);
let keyboard = false;

// The last input modality. A menu opened from the keyboard focuses its first
// item; one opened with the pointer only focuses the menu itself, so the arrow
// keys work at once without an item looking picked. Window capture phase: it
// has to be current before any handler that opens a menu runs.
if (typeof window !== "undefined") {
  window.addEventListener(
    "keydown",
    (event) => {
      if (!MODIFIERS.has(event.key)) keyboard = true;
    },
    true,
  );
  window.addEventListener("pointerdown", () => (keyboard = false), true);
  window.addEventListener("mousedown", () => (keyboard = false), true);
}

export function lastInputWasKeyboard(): boolean {
  return keyboard;
}

export function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

export function isEnabledItem(item: Element): boolean {
  return item.getAttribute("aria-disabled") !== "true" && !item.hasAttribute("disabled");
}

function menuItems(menu: HTMLElement): HTMLElement[] {
  return Array.from(menu.querySelectorAll<HTMLElement>(MENU_ITEM));
}

/**
 * The item a navigation key (ArrowDown, ArrowUp, Home, End) moves to from
 * `current` — an item, or null while the menu itself has focus. Skips disabled
 * items and wraps around; null for other keys or a menu without enabled items.
 */
export function menuItemFor(menu: HTMLElement, key: string, current: Element | null): HTMLElement | null {
  const items = menuItems(menu);
  if (key === "Home" || key === "End") {
    const enabled = items.filter(isEnabledItem);
    return (key === "Home" ? enabled[0] : enabled[enabled.length - 1]) ?? null;
  }
  const step = key === "ArrowDown" ? 1 : key === "ArrowUp" ? -1 : 0;
  if (step === 0) return null;
  const count = items.length;
  const from = current ? items.indexOf(current as HTMLElement) : -1;
  // Nothing focused yet: ArrowDown starts at the top, ArrowUp at the bottom.
  let index = from !== -1 ? from : step === 1 ? -1 : count;
  for (let tries = 0; tries < count; tries++) {
    index = (index + step + count) % count;
    if (isEnabledItem(items[index])) return items[index];
  }
  return null;
}

/** Type-ahead: the next enabled item after `current` whose label starts with `char`. */
export function typeaheadItem(menu: HTMLElement, char: string, current: Element | null): HTMLElement | null {
  const items = menuItems(menu).filter(isEnabledItem);
  const needle = char.toLocaleLowerCase();
  const from = current ? items.indexOf(current as HTMLElement) : -1;
  for (let offset = 1; offset <= items.length; offset++) {
    const item = items[(from + offset) % items.length];
    if (item.textContent?.trim().toLocaleLowerCase().startsWith(needle)) return item;
  }
  return null;
}

/** The keyboard's context-menu gesture: the Menu key or Shift+F10. */
export function isContextMenuKey(event: KeyboardEvent): boolean {
  if (event.ctrlKey || event.altKey || event.metaKey) return false;
  return event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey);
}

/** Where a menu for `element` opens when no pointer says where: under it, at its left edge. */
export function pointBelow(element: Element): { x: number; y: number } {
  const rect = element.getBoundingClientRect();
  return { x: rect.left, y: rect.bottom + 4 };
}

/**
 * Where to open a menu for a click on its trigger: at the pointer — or under
 * the trigger when the click came from the keyboard (Enter/Space clicks have
 * `detail` 0 and no coordinates, which would open the menu in the corner).
 */
export function menuPoint(event: MouseEvent): { x: number; y: number } {
  const trigger = event.currentTarget;
  if (event.detail === 0 && trigger instanceof Element) return pointBelow(trigger);
  return { x: event.clientX, y: event.clientY };
}

/**
 * Opens an element's right-click menu from the keyboard. On the Menu key or
 * Shift+F10 inside the element, it dispatches a `contextmenu` event positioned
 * under the element, so the element's existing `oncontextmenu` handler runs
 * unchanged. Attach it next to that handler; the key may come from the
 * element itself or from any focusable element inside it.
 *
 * preventDefault stops the WebView's own keyboard `contextmenu` for Shift+F10.
 * The Menu key's (Chromium sends it on key-up on Windows) then targets the
 * menu, which has taken focus by then and ignores it.
 */
export const contextMenuKey: Attachment<HTMLElement> = (node) => {
  const onKeydown = (event: KeyboardEvent) => {
    if (event.defaultPrevented || !isContextMenuKey(event) || isEditable(event.target)) return;
    event.preventDefault();
    const { x, y } = pointBelow(node);
    node.dispatchEvent(
      new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: x, clientY: y }),
    );
  };
  node.addEventListener("keydown", onKeydown);
  return () => node.removeEventListener("keydown", onKeydown);
};
