/**
 * Stand-in for SvelteKit's `$app/navigation` in the unit tests, which
 * vitest.config.ts aliases here: the real module comes from SvelteKit's Vite
 * plugin, and the tests do not load it. A test that cares where a component
 * navigates mocks this module.
 */
export async function goto(_url: string | URL, _options?: unknown): Promise<void> {}

/** No history in a test: nothing is captured or restored. */
export function snapshot<T>(_options: {
  id?: string;
  capture: () => T;
  restore: (value: T) => void;
  reset?: () => void;
}): void {}
