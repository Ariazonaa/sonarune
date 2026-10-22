import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";
import { svelte, vitePreprocess } from "@sveltejs/vite-plugin-svelte";

// Frontend unit tests (pure logic + Tauri `invoke` mocks). The svelte plugin
// lets `.svelte.ts` rune modules (e.g. the shortcut store) compile; the browser
// condition points Svelte at its client runtime so runes work under jsdom.
export default defineConfig({
  plugins: [svelte({ preprocess: vitePreprocess() })],
  // `#lib/...` resolves through the `imports` field in package.json (SvelteKit
  // 3 replaced the `$lib` alias with it), so no alias is needed here.
  resolve: {
    conditions: ["browser"],
    // `$app/*` comes from SvelteKit's Vite plugin, which the tests leave out:
    // a component that navigates, or a page that reads its route, gets a
    // stand-in (src/lib/testing), for its test to mock or set (see
    // ArtistLinks.test.ts, the playlist page's test).
    alias: {
      "$app/navigation": fileURLToPath(
        new URL("./src/lib/testing/appNavigation.ts", import.meta.url),
      ),
      "$app/state": fileURLToPath(new URL("./src/lib/testing/appState.ts", import.meta.url)),
    },
  },
  test: {
    environment: "jsdom",
    // The release and asset scripts carry their own tests next to them.
    include: ["src/**/*.{test,spec}.ts", "scripts/**/*.test.ts"],
    // A checkout on a network share imports so slowly that a full pool of
    // workers times out before it starts ("Timeout waiting for worker to
    // respond"), and the run fails with no test having run. Two workers get
    // through; CI on a local disk keeps the default.
    maxWorkers: process.env.CI ? undefined : 2,
  },
});
