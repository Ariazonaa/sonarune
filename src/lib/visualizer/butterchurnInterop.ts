type Butterchurn = (typeof import("butterchurn"))["default"];

/**
 * Find `createVisualizer` in the butterchurn module, however it was wrapped.
 *
 * Butterchurn is a webpack UMD bundle whose `module.exports` is
 * `{ __esModule: true, default: Butterchurn }`. Dev (esbuild's pre-bundle)
 * honours `__esModule` and hands over the class as the default export; the
 * production build (Rolldown) wraps `module.exports` once more, so the class
 * sits at `default.default` and `butterchurn.createVisualizer` is undefined —
 * the visualizer only broke in built apps. Same story as `unwrapPresets`.
 */
export function unwrapButterchurn(module: unknown): Butterchurn {
  let candidate = module;
  for (let depth = 0; depth < 3 && candidate; depth++) {
    if (typeof (candidate as Partial<Butterchurn>).createVisualizer === "function") {
      return candidate as Butterchurn;
    }
    candidate = (candidate as { default?: unknown }).default;
  }
  throw new Error("butterchurn: the module has no createVisualizer");
}
