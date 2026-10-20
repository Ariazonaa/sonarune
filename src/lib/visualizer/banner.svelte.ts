/** How long a notice stays up; the visualizer carries on underneath. */
export const NOTICE_MS = 6_000;

/**
 * The red banner over a visualizer surface (the visualizer route, the
 * projector). An error — the visuals cannot go on: creation failed, the
 * graphics context is gone — stays until it is dismissed. A notice — a preset
 * was skipped, online presets are unavailable, an import failed — hides
 * itself, and never covers an error. Clicking the banner dismisses either.
 */
export class VisualizerBanner {
  message = $state<string | null>(null);
  /** Whether `message` is an error rather than a notice. */
  isError = $state(false);
  #timer: ReturnType<typeof setTimeout> | undefined;

  error(message: string) {
    clearTimeout(this.#timer);
    this.isError = true;
    this.message = message;
  }

  notice(message: string) {
    if (this.isError) return;
    clearTimeout(this.#timer);
    this.isError = false;
    this.message = message;
    this.#timer = setTimeout(() => this.dismiss(), NOTICE_MS);
  }

  /** Take a notice down early (the action it was about succeeded after all). */
  clearNotice() {
    if (!this.isError) this.dismiss();
  }

  dismiss() {
    clearTimeout(this.#timer);
    this.#timer = undefined;
    this.isError = false;
    this.message = null;
  }
}
