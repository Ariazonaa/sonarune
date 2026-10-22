/**
 * Props a test can change after mounting a component: `mount` follows a
 * `$state` object, which a plain `.test.ts` file cannot create itself.
 */
export function liveProps<T extends object>(initial: T): T {
  const props = $state(initial);
  return props;
}
