import { afterEach, describe, expect, it } from "vitest";
import { controlOwnsKey, isTextField } from "#lib/controls.js";

function element(html: string): HTMLElement {
  document.body.insertAdjacentHTML("beforeend", html);
  return document.body.lastElementChild as HTMLElement;
}

const key = (k: string, init: KeyboardEventInit = {}) => new KeyboardEvent("keydown", { key: k, ...init });

afterEach(() => document.body.replaceChildren());

describe("isTextField", () => {
  it("is true for the fields that take typed text", () => {
    for (const html of ['<input type="text">', '<input type="search">', '<input type="number">', "<input>", "<textarea></textarea>"]) {
      expect(isTextField(element(html))).toBe(true);
    }
  });

  it("is false for sliders, checkboxes, selects and everything else", () => {
    for (const html of [
      '<input type="range">',
      '<input type="checkbox">',
      '<input type="radio">',
      '<input type="color">',
      "<select></select>",
      "<button></button>",
      "<div></div>",
    ]) {
      expect(isTextField(element(html))).toBe(false);
    }
    expect(isTextField(null)).toBe(false);
    expect(isTextField(window)).toBe(false);
  });
});

describe("controlOwnsKey", () => {
  it("gives a text field and a select every key", () => {
    for (const html of ['<input type="text">', "<textarea></textarea>", "<select></select>"]) {
      const target = element(html);
      for (const k of ["a", " ", "ArrowRight", "Home"]) expect(controlOwnsKey(target, key(k))).toBe(true);
    }
  });

  it("gives a slider its movement keys, but no chord and nothing else", () => {
    const slider = element('<input type="range">');
    for (const k of ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"]) {
      expect(controlOwnsKey(slider, key(k))).toBe(true);
    }
    expect(controlOwnsKey(slider, key("ArrowRight", { ctrlKey: true }))).toBe(false);
    expect(controlOwnsKey(slider, key(" "))).toBe(false);
    expect(controlOwnsKey(slider, key("m"))).toBe(false);
  });

  it("gives a checkbox Space, a radio Space and its arrows, a button-like input Space and Enter", () => {
    const checkbox = element('<input type="checkbox">');
    expect(controlOwnsKey(checkbox, key(" "))).toBe(true);
    expect(controlOwnsKey(checkbox, key("ArrowDown"))).toBe(false);
    expect(controlOwnsKey(checkbox, key("m"))).toBe(false);

    const radio = element('<input type="radio">');
    expect(controlOwnsKey(radio, key(" "))).toBe(true);
    expect(controlOwnsKey(radio, key("ArrowDown"))).toBe(true);
    expect(controlOwnsKey(radio, key("ArrowDown", { ctrlKey: true }))).toBe(false);

    const color = element('<input type="color">');
    expect(controlOwnsKey(color, key("Enter"))).toBe(true);
    expect(controlOwnsKey(color, key(" "))).toBe(true);
    expect(controlOwnsKey(color, key("ArrowLeft"))).toBe(false);
  });

  it("leaves keys on anything that is no form control to the shortcuts", () => {
    expect(controlOwnsKey(element("<button></button>"), key(" "))).toBe(false);
    expect(controlOwnsKey(element("<div></div>"), key("a"))).toBe(false);
    expect(controlOwnsKey(null, key("a"))).toBe(false);
  });
});
