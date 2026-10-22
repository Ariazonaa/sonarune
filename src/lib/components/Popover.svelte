<script lang="ts" module>
  /** Open popovers, each with the element focus returns to when it closes. */
  const openers = new Map<HTMLElement, HTMLElement | null>();

  /** The element that has focus as a popover opens. A popover opened from
   *  inside another one (a menu item that opens the playlist picker) takes
   *  over that popover's opener instead: the item is on its way out. */
  function focusedOpener(): HTMLElement | null {
    const active = document.activeElement;
    if (!(active instanceof HTMLElement) || active === document.body) return null;
    for (const [popover, opener] of openers) {
      if (popover.contains(active)) return opener;
    }
    return active;
  }
</script>

<script lang="ts">
  import { tick, untrack, type Snippet } from "svelte";
  import { focusableIn } from "#lib/modal.js";
  import { portal } from "#lib/portal.js";
  import {
    MENU_ITEM,
    isEditable,
    isEnabledItem,
    lastInputWasKeyboard,
    menuItemFor,
    typeaheadItem,
  } from "#lib/menu.js";

  interface Props {
    /** Open at a viewport point — context menus open at the cursor. */
    at?: { x: number; y: number } | null;
    /** Or next to an element (toolbar popovers). Presses on it never count as
     *  "outside", so its own toggle keeps working. */
    anchor?: HTMLElement | null;
    /** The button that opened a popover placed `at` a point (a menu button):
     *  like `anchor`, presses on it never count as "outside", so its click can
     *  close what it opened instead of the press closing it and the click
     *  opening it again. */
    trigger?: HTMLElement | null;
    /** Side of `anchor` to open on; flips when that side has no room. */
    placement?: "above" | "below";
    /** Which edge of `anchor` the popover lines up with. */
    align?: "start" | "end";
    /** `menu` gets the menu keyboard pattern (focus on open, arrow keys, …;
     *  see `onMenuKeydown`) and needs menuitem-role children. Popovers with
     *  richer content (form controls) are a `dialog`: opened with the pointer,
     *  focus stays put; opened from the keyboard, it moves to the first
     *  control (see `onDialogTab`). */
    role?: "menu" | "dialog";
    label?: string;
    class?: string;
    onclose: () => void;
    children: Snippet;
  }

  let {
    at = null,
    anchor = null,
    trigger = null,
    placement = "below",
    align = "start",
    role = "menu",
    label,
    class: className = "",
    onclose,
    children,
  }: Props = $props();

  /** Distance to the viewport edge, and to the anchor. */
  const MARGIN = 4;
  const GAP = 8;

  let el = $state<HTMLElement | null>(null);
  let size = $state({ w: 0, h: 0 });

  /** Where focus goes back to when the popover closes. */
  const opener = focusedOpener();
  /** What a popover opened at a point with the pointer belongs to: the
   *  element under that point (the right-clicked row), taken before the
   *  popover covers it. Not when opened from the keyboard: that menu opens
   *  just below the focused row (`pointBelow`), over whatever lies there —
   *  below the last visible row of a list, the player bar. It belongs to its
   *  opener, the row. */
  const pointOwner = untrack(() =>
    at && !anchor && !trigger && !lastInputWasKeyboard()
      ? (document.elementFromPoint?.(at.x, at.y) ?? null)
      : null,
  );
  /** A press elsewhere closed the popover: that press decides where focus goes. */
  let closedByPress = false;

  // Track the size, not just the first measurement: content that loads after
  // opening (a playlist list) must still be kept inside the viewport.
  function measure(node: HTMLElement) {
    const update = () => (size = { w: node.offsetWidth, h: node.offsetHeight });
    update();
    const observer = new ResizeObserver(update);
    observer.observe(node);
    return () => observer.disconnect();
  }

  // A menu takes focus as it opens — after a tick, because the popover is
  // `visibility: hidden` until measured and a hidden element can't be
  // focused. Opened from the keyboard, the first enabled item gets focus;
  // opened with the pointer, the menu itself does, so no item looks picked
  // but the arrow keys work at once. A dialog opened from the keyboard hands
  // focus to its first control: portaled to the end of <body>, it would
  // otherwise sit behind the rest of the page in the tab order. On close, a
  // popover that still holds focus hands it back to its opener.
  function manageFocus(node: HTMLElement) {
    openers.set(node, opener);
    let open = true;
    void tick().then(() => {
      if (!open || node.contains(document.activeElement)) return;
      if (role === "menu") {
        const first = lastInputWasKeyboard() ? menuItemFor(node, "Home", null) : null;
        (first ?? node).focus({ preventScroll: true });
      } else if (lastInputWasKeyboard()) {
        (focusableIn(node)[0] ?? node).focus({ preventScroll: true });
      }
    });
    return () => {
      open = false;
      openers.delete(node);
      const active = document.activeElement;
      const focusLost = !active || active === document.body || node.contains(active);
      if (focusLost && !closedByPress && opener?.isConnected) opener.focus({ preventScroll: true });
    };
  }

  const pos = $derived.by(() => {
    const { w, h } = size;
    let left = at?.x ?? 0;
    let top = at?.y ?? 0;
    if (anchor) {
      // Read once per layout pass; the popover closes on resize and on any
      // scroll that could move the anchor, so it cannot move underneath it.
      const r = anchor.getBoundingClientRect();
      left = align === "end" ? r.right - w : r.left;
      const above = r.top - GAP - h;
      const below = r.bottom + GAP;
      const fitsAbove = above >= MARGIN;
      const fitsBelow = below + h <= window.innerHeight - MARGIN;
      top = placement === "above" ? (fitsAbove || !fitsBelow ? above : below) : fitsBelow || !fitsAbove ? below : above;
    }
    return {
      left: Math.max(MARGIN, Math.min(left, window.innerWidth - w - MARGIN)),
      top: Math.max(MARGIN, Math.min(top, window.innerHeight - h - MARGIN)),
    };
  });

  function inside(target: EventTarget | null, withAnchor: boolean): boolean {
    if (!(target instanceof Node)) return false;
    if (el?.contains(target)) return true;
    return withAnchor && (!!anchor?.contains(target) || !!trigger?.contains(target));
  }

  function pressed(target: EventTarget | null) {
    if (inside(target, true)) return;
    closedByPress = true;
    onclose();
  }

  /**
   * A scroll elsewhere closes the popover only when it can move what the
   * popover belongs to — its anchor, its trigger, the content under its
   * point, or failing those its opener — and so leave it floating next to
   * nothing. The app scrolls panels by itself all the time (the lyrics follow
   * the song, the queue reveals the playing row): a menu or a half-typed
   * playlist name must not vanish under the pointer because a panel that
   * holds neither moved. An owner that has left the page (its row was
   * rendered anew) no longer tells which scroll moves it, so then any scroll
   * closes the popover, as one with no owner at all.
   */
  function scrolled(target: EventTarget | null) {
    if (inside(target, false)) return;
    const owner = anchor ?? trigger ?? pointOwner ?? opener;
    if (owner?.isConnected && target instanceof Node && !target.contains(owner)) return;
    onclose();
  }

  /**
   * Keys while focus is inside a menu (WAI-ARIA menu pattern):
   * - ↑/↓ move between enabled items and wrap around; Home/End jump to the
   *   first/last one. From the menu itself (pointer open), ↓ starts at the
   *   top and ↑ at the bottom.
   * - Enter/Space activate the focused item; a letter jumps to the next item
   *   starting with it. ←/→ do nothing (no submenus).
   * - Escape (handled for every popover) closes; focus returns to the opener.
   * - Tab closes, puts focus back on the opener and lets Tab's default action
   *   carry on from there, so focus lands next to (Shift+Tab: before) the
   *   element the menu was opened from — as when leaving a menu button's
   *   menu. Tabbing on from the menu itself would jump from the end of
   *   <body>, far away from where the user was.
   * While a menu has focus it owns the keyboard: its keys stop here, so no
   * page handler or global shortcut reacts. An inline text field inside a
   * menu (the new-playlist name) keeps its keys.
   */
  function onMenuKeydown(event: KeyboardEvent, menu: HTMLElement) {
    if (event.key === "Tab") {
      if (opener?.isConnected) opener.focus({ preventScroll: true });
      onclose();
      return;
    }
    const target = event.target as HTMLElement;
    if (isEditable(target)) return;
    event.stopPropagation();
    if (event.ctrlKey || event.altKey || event.metaKey) return;
    const current = target.closest<HTMLElement>(MENU_ITEM);
    let next: HTMLElement | null;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp":
      case "Home":
      case "End":
        next = menuItemFor(menu, event.key, current);
        break;
      case "Enter":
      case " ":
        // Activate by hand, so a non-button item works the same and the
        // button's own key activation can't fire a second click.
        if (current || target === menu) event.preventDefault();
        if (current && isEnabledItem(current)) current.click();
        return;
      case "ArrowLeft":
      case "ArrowRight":
        event.preventDefault();
        return;
      default:
        if (event.key.length !== 1) return;
        next = typeaheadItem(menu, event.key, current);
    }
    event.preventDefault();
    next?.focus();
  }

  /**
   * Tab inside a dialog popover moves between its controls as usual. Past the
   * last one it closes and puts focus back on the opener, and Tab carries on
   * from there; Shift+Tab before the first one closes and stays on the
   * opener. The popover belongs right after its opener in the tab order, even
   * though it is portaled to the end of <body> — Tab must neither run off
   * into the rest of the page with the popover still open, nor leave the
   * user at the end of the document.
   */
  function onDialogTab(event: KeyboardEvent, dialog: HTMLElement) {
    const controls = focusableIn(dialog);
    const target = event.target;
    const leaving = event.shiftKey
      ? target === dialog || target === controls[0]
      : controls.length === 0 || target === controls[controls.length - 1];
    if (!leaving) return;
    if (opener?.isConnected) {
      opener.focus({ preventScroll: true });
      if (event.shiftKey) event.preventDefault();
    }
    onclose();
  }
</script>

<!-- Any press, right-click or resize elsewhere dismisses it; mousedown (not
     click) so the very next press closes it immediately. So does a scroll
     that can move what it belongs to (see `scrolled`); scrolling inside the
     popover (a long list) does not. -->
<svelte:window
  onmousedowncapture={(e) => pressed(e.target)}
  oncontextmenucapture={(e) => pressed(e.target)}
  onscrollcapture={(e) => scrolled(e.target)}
  onresize={onclose}
  onkeydowncapture={(e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    } else if (!el || !inside(e.target, false)) {
      return;
    } else if (role === "menu") {
      onMenuKeydown(e, el);
    } else if (e.key === "Tab") {
      onDialogTab(e, el);
    }
  }}
/>

<!-- Stays in place so Svelte's block bookkeeping never sees the moved node. -->
<div class="contents">
  <div
    bind:this={el}
    {@attach portal}
    {@attach measure}
    {@attach manageFocus}
    class="overlay fixed z-50 rounded-overlay p-1 outline-none {className}"
    style:left="{pos.left}px"
    style:top="{pos.top}px"
    style:visibility={size.w > 0 ? "visible" : "hidden"}
    {role}
    aria-label={label}
    tabindex="-1"
    oncontextmenu={(e) => e.preventDefault()}
  >
    {@render children()}
  </div>
</div>
