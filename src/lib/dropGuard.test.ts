import { afterEach, describe, expect, it } from "vitest";
import { guardDrops } from "./dropGuard";
import { isTextField } from "./controls";

function dispatch(target: EventTarget, type: string): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}

describe("the drop guard", () => {
  let release = () => {};
  afterEach(() => {
    release();
    document.body.innerHTML = "";
  });

  it("swallows a drop that nothing in the app took", () => {
    release = guardDrops(window, isTextField);
    const area = document.body.appendChild(document.createElement("div"));
    expect(dispatch(area, "dragover").defaultPrevented).toBe(true);
    expect(dispatch(area, "drop").defaultPrevented).toBe(true);
  });

  it("leaves a drop to the target that took it", () => {
    release = guardDrops(window, isTextField);
    const target = document.body.appendChild(document.createElement("div"));
    let seen: boolean | null = null;
    target.addEventListener("drop", (event) => event.preventDefault());
    window.addEventListener("drop", (event) => (seen = event.defaultPrevented), { once: true });
    expect(dispatch(target, "drop").defaultPrevented).toBe(true);
    // The target, not the guard, handled it: still prevented once, as before.
    expect(seen).toBe(true);
  });

  it("lets text fields take dropped text", () => {
    release = guardDrops(window, isTextField);
    const input = document.body.appendChild(document.createElement("input"));
    expect(dispatch(input, "dragover").defaultPrevented).toBe(false);
    expect(dispatch(input, "drop").defaultPrevented).toBe(false);
  });

  it("stops guarding once released", () => {
    guardDrops(window, isTextField)();
    const area = document.body.appendChild(document.createElement("div"));
    expect(dispatch(area, "drop").defaultPrevented).toBe(false);
  });
});
