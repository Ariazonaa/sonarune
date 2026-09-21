import { afterEach, describe, expect, it, vi } from "vitest";

// The overlay guard never touches playback; keep the Tauri-backed store out.
vi.mock("$lib/state/player.svelte", () => ({ player: {} }));

import { blockedByOverlay } from "./shortcuts";
import { isModalOpen } from "./modal";
import { confirm } from "$lib/state/confirm.svelte";
import { help } from "$lib/state/help.svelte";
import { palette } from "$lib/state/palette.svelte";

function addDialog(modal: boolean) {
  const element = document.createElement("div");
  element.setAttribute("role", "dialog");
  if (modal) element.setAttribute("aria-modal", "true");
  document.body.append(element);
}

function askConfirm() {
  void confirm.ask({ title: "t", body: "b", confirmLabel: "ok" });
}

afterEach(() => {
  confirm.answer(false);
  palette.close();
  help.close();
  document.body.replaceChildren();
});

describe("isModalOpen", () => {
  it("sees an aria-modal dialog but not a non-modal popover", () => {
    expect(isModalOpen()).toBe(false);
    addDialog(false);
    expect(isModalOpen()).toBe(false);
    addDialog(true);
    expect(isModalOpen()).toBe(true);
  });

  it("counts a pending confirmation", () => {
    askConfirm();
    expect(isModalOpen()).toBe(true);
  });
});

describe("blockedByOverlay", () => {
  it("lets shortcuts through with nothing open", () => {
    expect(blockedByOverlay("playPause")).toBe(false);
  });

  it("blocks every shortcut while a modal dialog is open", () => {
    addDialog(true);
    expect(blockedByOverlay("playPause")).toBe(true);
    expect(blockedByOverlay("palette")).toBe(true);
  });

  it("blocks even the palette toggle while a confirmation is pending", () => {
    palette.show();
    askConfirm();
    expect(blockedByOverlay("palette")).toBe(true);
  });

  it("leaves the palette and the help overlay their own toggle", () => {
    palette.show();
    addDialog(true);
    expect(blockedByOverlay("palette")).toBe(false);
    expect(blockedByOverlay("next")).toBe(true);

    palette.close();
    help.toggle();
    expect(blockedByOverlay("help")).toBe(false);
    expect(blockedByOverlay("mute")).toBe(true);
  });
});
