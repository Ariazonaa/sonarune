import { defineConfig } from "vitest/config";
import { svelte, vitePreprocess } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

// Frontend unit tests (pure logic + Tauri `invoke` mocks). The svelte plugin
// lets `.svelte.ts` rune modules (e.g. the shortcut store) compile; the browser
// condition points Svelte at its client runtime so runes work under jsdom.
export default defineConfig({
  plugins: [svelte({ preprocess: vitePreprocess() })],
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL("./src/lib", import.meta.url)),
    },
    conditions: ["browser"],
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.{test,spec}.ts"],
    // A checkout on a network share imports so slowly that a full pool of
    // workers times out before it starts ("Timeout waiting for worker to
    // respond"), and the run fails with no test having run. Two workers get
    // through; CI on a local disk keeps the default.
    maxWorkers: process.env.CI ? undefined : 2,
  },
});
