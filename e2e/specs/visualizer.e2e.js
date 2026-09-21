// Visualizer mount/teardown — one of the Phase-13 GUI core flows. Runs whenever
// the app is signed in: on this dev machine via the restored session, or in CI
// after signing in with JELLYSIC_TEST_{URL,USER,PASS}. If it can't reach the
// signed-in shell (signed out and no creds), the whole spec skips.
//
// The visualizer route's onMount spins up the Butterchurn/WebGL controller and
// the PCM tap; navigating away must run its teardown (controller.destroy + tap
// unsubscribe) without leaving a player error banner behind.

import { ensureSignedIn } from "../helpers.js";

describe("visualizer mount/teardown", function () {
  before(async function () {
    if (!(await ensureSignedIn())) {
      // eslint-disable-next-line no-invalid-this
      this.skip();
    }
  });

  it("opens the visualizer with a canvas", async () => {
    const toggle = await $('[data-testid="visualizer-toggle"]');
    await toggle.waitForExist({ timeout: 15000 });
    await toggle.click();

    const visualizer = await $('[data-testid="visualizer"]');
    await visualizer.waitForExist({ timeout: 15000 });
    await expect(await visualizer.$("canvas")).toBeExisting();
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
