/**
 * What a focused form control does with a key. The global shortcuts and the
 * root layout's context-menu suppression share it, so both agree on what
 * counts as typing: a slider or a checkbox is no text field, and keeping focus
 * after a click (a dragged volume slider does) must not turn every shortcut
 * off.
 */

/** Input types that take typed text — and with it every key. */
const TEXT_TYPES = new Set([
  "text",
  "search",
  "email",
  "url",
  "tel",
  "password",
  "number",
  "date",
  "datetime-local",
  "month",
  "time",
  "week",
]);

/** The keys a slider moves with. */
const SLIDER_KEYS = new Set([
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "ArrowDown",
  "Home",
  "End",
  "PageUp",
  "PageDown",
]);

/** Input types a press activates, like a button. */
const BUTTON_TYPES = new Set(["button", "submit", "reset", "image", "color", "file"]);

/**
 * Whether `target` takes typed text: a text-like `<input>`, a `<textarea>` or
 * a contenteditable element. Only these keep the WebView's own right-click
 * menu (copy/paste).
 */
export function isTextField(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target instanceof HTMLTextAreaElement || target.isContentEditable) return true;
  return target instanceof HTMLInputElement && TEXT_TYPES.has(target.type);
}

/**
 * Whether the focused control `target` uses `event`'s key itself, so no global
 * shortcut may take it: a text field or a `<select>` every key; a slider its
 * arrows, Home/End and PageUp/PageDown, but not with Ctrl, Alt or ⌘ held
 * (Ctrl+→ stays "next track"); a checkbox Space, a radio button Space and its
 * arrows; a button-like input Space and Enter. Everything else a slider or a
 * checkbox gets — Space, letters, chords — is left to the shortcuts.
 */
export function controlOwnsKey(target: EventTarget | null, event: KeyboardEvent): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (isTextField(target) || target instanceof HTMLSelectElement) return true;
  if (!(target instanceof HTMLInputElement)) return false;
  const chord = event.ctrlKey || event.altKey || event.metaKey;
  if (target.type === "range") return !chord && SLIDER_KEYS.has(event.key);
  if (target.type === "checkbox") return event.key === " ";
  if (target.type === "radio") return event.key === " " || (!chord && event.key.startsWith("Arrow"));
  if (BUTTON_TYPES.has(target.type)) return event.key === " " || event.key === "Enter";
  // An input type this list does not know: treat it as a text field, as
  // before, rather than take keys away from it.
  return true;
}
