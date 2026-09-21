import { flushSync, mount, unmount } from "svelte";
import { afterEach, describe, expect, it } from "vitest";
import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";
import { confirm } from "$lib/state/confirm.svelte";

// The ConfirmDialog is the safety net in front of destructive server writes
// (playlist/track deletion). Its whole reason to exist is that a reflexive
// Enter must NOT be enough to confirm — only an explicit click of the danger
// button, or Escape/backdrop to back out. These tests pin exactly that focus
// and keyboard contract. Rendered with Svelte 5's own mount/flushSync (no
// extra testing library), driving the shared `confirm` store the way the app
// does via `confirm.ask(...)`.

let instance: Record<string, unknown> | null = null;

function render() {
  instance = mount(ConfirmDialog, { target: document.body });
  flushSync();
}

/** Open a destructive prompt exactly like a delete flow does. */
function askDestructive() {
  const p = confirm.ask({
    title: "Delete playlist",
    body: "This removes it from the server.",
    confirmLabel: "Delete",
    danger: true,
  });
  flushSync();
  return p;
}

function dialog() {
  return document.querySelector('[role="dialog"]');
}

function dialogButtons() {
  return [...document.querySelectorAll('[role="dialog"] button')] as HTMLButtonElement[];
}

afterEach(() => {
  // The confirm store is a module-level singleton — resolve any dangling
  // request so it can't leak into the next test, then tear the component down.
  confirm.answer(false);
  if (instance) {
    unmount(instance);
    instance = null;
  }
  document.body.innerHTML = "";
});

describe("ConfirmDialog", () => {
  it("renders nothing until a request is asked", () => {
    render();
    expect(dialog()).toBeNull();
  });

  it("puts its overlay in <body>, not where it is declared", () => {
    // It is asked for by dialogs that are themselves portaled to <body>. Left
    // where the layout declares it, it painted *behind* the dialog that had
    // asked — same z-index, earlier in the document — so the prompt sat under
    // that dialog's backdrop and pressing Save looked like it did nothing.
    const host = document.createElement("div");
    document.body.appendChild(host);
    instance = mount(ConfirmDialog, { target: host });
    flushSync();
    askDestructive();

    const overlay = dialog()?.parentElement;
    expect(overlay).not.toBeNull();
    expect(overlay?.parentElement).toBe(document.body);
    expect(host.contains(overlay!)).toBe(false);
  });

  it("opens a modal dialog for a confirm request", () => {
    render();
    askDestructive();
    const d = dialog();
    expect(d).not.toBeNull();
    expect(d?.getAttribute("aria-modal")).toBe("true");
  });

  it("moves initial focus to the cancel button, never the destructive one", () => {
    render();
    askDestructive();
    const [cancel, danger] = dialogButtons();
    // Cancel is rendered first, the danger button second.
    expect(document.activeElement).toBe(cancel);
    expect(document.activeElement).not.toBe(danger);
  });

  it("Enter does NOT confirm a destructive dialog", async () => {
    render();
    const p = askDestructive();
    let settled: boolean | "pending" = "pending";
    void p.then((v) => (settled = v));

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    flushSync();
    // Let any microtask that a stray handler might have queued run.
    await Promise.resolve();

    expect(settled).toBe("pending");
    expect(dialog()).not.toBeNull();

    // Backing out still works and resolves declined.
    confirm.answer(false);
    await expect(p).resolves.toBe(false);
  });

  it("Escape resolves the request as declined and closes it", async () => {
    render();
    const p = askDestructive();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    flushSync();
    await expect(p).resolves.toBe(false);
    expect(dialog()).toBeNull();
  });

  it("clicking the confirm button resolves true", async () => {
    render();
    const p = askDestructive();
    const [, danger] = dialogButtons();
    danger.click();
    flushSync();
    await expect(p).resolves.toBe(true);
    expect(dialog()).toBeNull();
  });

  it("clicking the backdrop declines", async () => {
    render();
    const p = askDestructive();
    // The dialog's parent is the full-screen overlay; a click whose target is
    // the overlay itself (not the panel) backs out.
    const overlay = dialog()?.parentElement as HTMLElement;
    overlay.click();
    flushSync();
    await expect(p).resolves.toBe(false);
    expect(dialog()).toBeNull();
  });

  it("a second request cancels the first as declined", async () => {
    render();
    const first = confirm.ask({ title: "A", body: "a", confirmLabel: "ok" });
    const second = confirm.ask({ title: "B", body: "b", confirmLabel: "ok" });
    flushSync();

    await expect(first).resolves.toBe(false);
    expect(confirm.request?.title).toBe("B");

    confirm.answer(false);
    await expect(second).resolves.toBe(false);
  });
});
