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
  },
});
