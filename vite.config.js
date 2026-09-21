import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";
import { paraglideVitePlugin } from "@inlang/paraglide-js";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    paraglideVitePlugin({
      project: "./project.inlang",
      outdir: "./src/lib/paraglide",
      // SPA without URL locales: remembered choice, then browser language.
      // Keep in sync with `i18n:compile` in package.json — `npm run check`
      // regenerates the same runtime and would otherwise swap in the
      // default strategy (cookie-based: the app falls back to English).
      strategy: ["localStorage", "preferredLanguage", "baseLocale"],
    }),
    tailwindcss(),
    sveltekit(),
  ],

  // Pre-bundle deps that are only imported lazily (visualizer routes).
  // Without this, the first navigation there makes Vite discover them,
  // re-optimize, and force a full page reload — white flash + dead click.
  optimizeDeps: {
    include: [
      "butterchurn",
      "butterchurn-presets",
      "butterchurn-presets/lib/butterchurnPresetsExtra.min.js",
      "blurhash",
      "@tauri-apps/api/core",
      "@tauri-apps/api/event",
    ],
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
      // 4. `JELLYSIC_POLL_WATCH=1` watches by polling instead of by OS events.
      //    On a checkout that lives on a network share, the native watcher
      //    dies with `ETIMEDOUT: connection timed out, watch` the moment the
      //    share stalls for a second — and it takes the whole dev server with
      //    it, before it ever answers on port 1420. Polling costs a little CPU
      //    and survives that. Off by default: on a local disk the OS events
      //    are both faster and free.
      usePolling: process.env.JELLYSIC_POLL_WATCH === "1",
      interval: 400,
    },
  },
}));
