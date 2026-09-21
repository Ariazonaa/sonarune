// A single, promise-based confirmation dialog shared app-wide. Call
// `confirm.ask(...)` and await the boolean; a `<ConfirmDialog>` mounted in the
// root layout renders the request. Used for destructive actions (server-side
// deletion) where a reflexive Enter shouldn't be enough — the dialog only
// resolves true on an explicit click of the danger button.
interface ConfirmRequest {
  title: string;
  body: string;
  confirmLabel: string;
  /** Style the confirm button as destructive (red). */
  danger?: boolean;
}

class ConfirmStore {
  request = $state<ConfirmRequest | null>(null);
  #resolve: ((ok: boolean) => void) | null = null;

  ask(request: ConfirmRequest): Promise<boolean> {
    // A new prompt before the previous one is answered cancels the previous.
    this.#resolve?.(false);
    this.request = request;
    return new Promise((resolve) => {
      this.#resolve = resolve;
    });
  }

  answer(ok: boolean) {
    this.request = null;
    const resolve = this.#resolve;
    this.#resolve = null;
    resolve?.(ok);
  }
}

export const confirm = new ConfirmStore();
