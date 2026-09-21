// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

// Output stays in the project `build/` dir: Tauri's `frontendDist` embeds it
// into the binary at build time, and an absolute path outside the project
// breaks that embedding (the installed app then serves an empty/dir listing).
// The ENOTEMPTY the adapter's rimraf occasionally hit on the SMB share is
// handled by the retry-capable pre-clean in `npm run build` (clean-build.mjs).
/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      fallback: "index.html",
    }),
  },
};

export default config;
