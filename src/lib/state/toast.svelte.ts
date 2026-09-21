export type ToastKind = "success" | "info" | "error";

interface Toast {
  id: number;
  message: string;
  kind: ToastKind;
}

interface ToastOptions {
  /** "success" (default): a finished action. "info": a neutral notice (a
   *  device fallback, a queued download). "error": a failure that needs no
   *  follow-up — anything the user must act on keeps the red banner
   *  (`player.error`). */
  kind?: ToastKind;
  /** How long it stays; errors default to longer, they take longer to read. */
  ms?: number;
}

const DEFAULT_MS = 2600;
const ERROR_MS = 5000;

/** Transient feedback pills above the player bar (`Toaster.svelte`). */
class ToastStore {
  toasts = $state<Toast[]>([]);
  #nextId = 0;

  show(message: string, { kind = "success", ms }: ToastOptions = {}) {
    const id = this.#nextId++;
    this.toasts = [...this.toasts, { id, message, kind }];
    setTimeout(() => this.dismiss(id), ms ?? (kind === "error" ? ERROR_MS : DEFAULT_MS));
  }

  dismiss(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }
}

export const toast = new ToastStore();
