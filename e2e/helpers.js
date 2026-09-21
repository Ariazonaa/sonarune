// Shared helpers for the E2E specs. wdio injects `$`, `browser`, etc. as
// globals, so they're available here without imports.

/**
 * Make sure the app is at the signed-in shell.
 * - Restored session (this dev machine): already there → true.
 * - Setup screen showing AND JELLYSIC_TEST_{URL,USER,PASS} set (CI): sign in.
 * - Otherwise (signed out, no creds): returns false so the caller can skip.
 */
export async function ensureSignedIn() {
  const shell = await $('[data-testid="app-shell"]');
  const setup = await $('[data-testid="setup-screen"]');

  // Restoring a session is a round trip to the server, so right after launch
  // neither view exists yet -- the layout is still showing "restoring". Asking
  // straight away made this return false on exactly the machine that HAS a
  // session, which silently skipped every spec built on it.
  await browser.waitUntil(
    async () => (await shell.isExisting()) || (await setup.isExisting()),
    { timeout: 30000, timeoutMsg: "neither the app shell nor the setup screen appeared" },
  );
  if (await shell.isExisting()) return true;

  const url = process.env.JELLYSIC_TEST_URL;
  const user = process.env.JELLYSIC_TEST_USER;
  const pass = process.env.JELLYSIC_TEST_PASS;
  if (!(await setup.isExisting()) || !url || !user || !pass) return false;

  await (await $('[data-testid="setup-server"]')).setValue(url);
  await (await $('[data-testid="setup-username"]')).setValue(user);
  await (await $('[data-testid="setup-password"]')).setValue(pass);
  await (await $('[data-testid="setup-connect"]')).click();
  await shell.waitForExist({ timeout: 30000 });
  return true;
}
