// Visualizer start/teardown — one of the Phase-13 GUI core flows. Runs whenever
// the app is signed in: via a session restored from an earlier e2e run, or in CI
// after signing in with SONARUNE_TEST_{URL,USER,PASS}. If it can't reach the
// signed-in shell (signed out and no creds), the whole spec skips.
//
// The visualizer route's onMount spins up the Butterchurn/WebGL controller and
// the PCM tap; navigating away must run its teardown (controller.destroy + tap
// unsubscribe) without leaving a player error banner behind.
//
// The canvas is static markup, there with or without a working visualizer. That
// it started is the controller putting a first preset on screen (`data-ready`)
// without the route's own error banner — the 0.6.0 build, where butterchurn
// never started ("createVisualizer is not a function"), passed a canvas check.

import { ensureSignedIn } from "../helpers.js";

describe("visualizer mount/teardown", function () {
  before(async function () {
    if (!(await ensureSignedIn())) {
      // eslint-disable-next-line no-invalid-this
      this.skip();
    }
  });

  it("starts the visualizer: a first preset loads, no error", async () => {
    const toggle = await $('[data-testid="visualizer-toggle"]');
    await toggle.waitForExist({ timeout: 15000 });
    await toggle.click();

    const visualizer = await $('[data-testid="visualizer"]');
    await visualizer.waitForExist({ timeout: 15000 });
    await expect(await visualizer.$("canvas")).toBeExisting();

    // A skipped preset is only a notice (`visualizer-notice`) and the next one
    // loads; an error means the visualizer could not start or went dark.
    const error = await $('[data-testid="visualizer-error"]');
    await browser.waitUntil(
      async () =>
        (await visualizer.getAttribute("data-ready")) === "true" || (await error.isExisting()),
      { timeout: 20000, timeoutMsg: "the visualizer neither loaded a preset nor reported an error" },
    );
    await expect(error).not.toBeExisting();
    await expect(visualizer).toHaveAttribute("data-ready", "true");
  });

  it("tears down cleanly when navigating away", async () => {
    // Toggle again → navigate back to the previous view.
    const toggle = await $('[data-testid="visualizer-toggle"]');
    await toggle.click();

    const visualizer = await $('[data-testid="visualizer"]');
    await visualizer.waitForExist({ timeout: 15000, reverse: true });
    await expect(await $('[data-testid="app-shell"]')).toBeExisting();

    // No player/visualizer error banner left behind after teardown. Anchored
    // on a testid, not on a color class: the banner was restyled once and this
    // assertion silently stopped matching anything instead of failing.
    await expect(await $('[data-testid="player-error"]')).not.toBeExisting();
  });
});
