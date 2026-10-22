/**
 * Stand-in for SvelteKit's `$app/state` in the unit tests, which
 * vitest.config.ts aliases here (the real module comes from SvelteKit's Vite
 * plugin). A test of a page sets the route's params before it mounts it.
 */
export const page: { params: Record<string, string | undefined>; url: URL } = {
  params: {},
  url: new URL("http://localhost/"),
};
