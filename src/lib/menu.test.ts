import { afterEach, describe, expect, it } from "vitest";
import { menuPoint } from "#lib/menu.js";

// menuPoint reads `currentTarget`, which exists only while the event is being
// dispatched: call it from a listener on the trigger, as a page handler does.
function pointFor(event: MouseEvent): { x: number; y: number } {
  const trigger = document.createElement("button");
  trigger.getBoundingClientRect = () => new DOMRect(120, 280, 200, 20);
  document.body.appendChild(trigger);
  let point: { x: number; y: number } | null = null;
  trigger.addEventListener(event.type, (e) => (point = menuPoint(e as MouseEvent)));
  trigger.dispatchEvent(event);
  return point!;
}

afterEach(() => document.body.replaceChildren());

describe("menuPoint", () => {
  it("opens at the pointer for a mouse click", () => {
    expect(pointFor(new MouseEvent("click", { detail: 1, clientX: 300, clientY: 200 }))).toEqual({ x: 300, y: 200 });
  });

  it("opens under the trigger for a click from the keyboard", () => {
    expect(pointFor(new MouseEvent("click", { detail: 0 }))).toEqual({ x: 120, y: 304 });
  });

  it("opens at the pointer for a right-click, whose detail is always 0", () => {
    // Chromium sends a mouse right-click's contextmenu with detail 0; taken
    // for a keyboard press, the menu opened at the row's far left edge.
    expect(pointFor(new MouseEvent("contextmenu", { detail: 0, clientX: 850, clientY: 200 }))).toEqual({
      x: 850,
      y: 200,
    });
  });
});
