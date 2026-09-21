import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "$lib/state/toast.svelte";

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  vi.runOnlyPendingTimers();
  vi.useRealTimers();
  toast.toasts = [];
});

describe("toast", () => {
  it("defaults to a short success toast", () => {
    toast.show("Added to playlist");
    expect(toast.toasts).toEqual([
      expect.objectContaining({ message: "Added to playlist", kind: "success" }),
    ]);
    vi.advanceTimersByTime(2599);
    expect(toast.toasts).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(toast.toasts).toHaveLength(0);
  });

  it("keeps errors on screen longer unless a duration is given", () => {
    toast.show("Could not reach the server", { kind: "error" });
    toast.show("Output device changed", { kind: "info", ms: 1000 });
    vi.advanceTimersByTime(1000);
    expect(toast.toasts.map((t) => t.kind)).toEqual(["error"]);
    vi.advanceTimersByTime(4000);
    expect(toast.toasts).toHaveLength(0);
  });

  it("dismisses exactly the clicked toast", () => {
    toast.show("a");
    toast.show("b");
    const [first] = toast.toasts;
    toast.dismiss(first.id);
    expect(toast.toasts.map((t) => t.message)).toEqual(["b"]);
  });
});
