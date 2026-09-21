// Boot smoke: proves the whole pipeline works — the real jellysic.exe launches
// under WebDriver, WebView2 loads the embedded SvelteKit build, and the SPA
// mounts and reaches a known-good landing view.
//
// The landing view depends on the environment: a machine with a persisted
// session restores straight into the signed-in shell; a clean machine (or CI)
// lands on the setup/sign-in screen. Either one proves the WebView loaded and
// the SPA mounted — a blank or errored WebView shows neither, which is the
// failure this catches. Needs no Jellyfin server.

/** True once the SPA has mounted into one of its two top-level shells. */
async function mountedShell() {
  const setup = await $('[data-testid="setup-screen"]');
  const shell = await $('[data-testid="app-shell"]');
  if (await setup.isExisting()) return "setup";
  if (await shell.isExisting()) return "shell";
  return null;
}

describe("app boot", () => {
  it("launches and mounts the SvelteKit SPA", async () => {
    await browser.waitUntil(async () => (await mountedShell()) !== null, {
      timeout: 30000,
      timeoutMsg: "neither the setup screen nor the app shell mounted — WebView blank?",
    });
  });

  it("renders an interactive view (not a blank/errored WebView)", async () => {
    const where = await mountedShell();
    expect(where).not.toBe(null);

    if (where === "setup") {
      // Signed out: the sign-in form must be usable.
      await expect(await $('[data-testid="setup-server"]')).toBeExisting();
      await expect(await $('[data-testid="setup-connect"]')).toBeExisting();
    } else {
      // Signed in (restored session): the shell chrome must be present.
      await expect(await $('[data-testid="app-shell"]')).toBeExisting();
      await expect(await $('[data-testid="visualizer-toggle"]')).toBeExisting();
    }

    const bodyText = await $("body").getText();
    expect(bodyText.length).toBeGreaterThan(0);
  });
});
