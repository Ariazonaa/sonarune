export const SHORTCUT_ACTIONS = [
  "palette",
  "help",
  "playPause",
  "next",
  "previous",
  "seekForward",
  "seekBackward",
  "volumeUp",
  "volumeDown",
  "mute",
  "favorite",
  "shuffle",
  "repeat",
] as const;

export type ShortcutAction = (typeof SHORTCUT_ACTIONS)[number];
type ShortcutBindings = Record<ShortcutAction, string[]>;

const DEFAULT_SHORTCUTS: ShortcutBindings = {
  palette: ["Mod+K", "Mod+P"],
  help: ["?"],
  playPause: ["Space"],
  next: ["N", "Mod+ArrowRight"],
  previous: ["P", "Mod+ArrowLeft"],
  seekForward: ["ArrowRight"],
  seekBackward: ["ArrowLeft"],
  volumeUp: ["ArrowUp"],
  volumeDown: ["ArrowDown"],
  mute: ["M"],
  favorite: ["L"],
  shuffle: ["S"],
  repeat: ["R"],
};

const STORAGE_KEY = "jellysic.shortcuts.v1";
const PURE_MODIFIERS = new Set(["Control", "Shift", "Alt", "Meta", "AltGraph"]);
const RESERVED = new Set([
  "F5",
  "F11",
  "Alt+F4",
  "Mod+A",
  "Mod+C",
  "Mod+V",
  "Mod+X",
  "Mod+Z",
  "Mod+Y",
  "Mod+L",
  "Mod+R",
  "Mod+T",
  "Mod+W",
  "Mod+N",
  "Mod+Q",
  "Mod+H",
  "Mod+Space",
]);

function cloneDefaults(): ShortcutBindings {
  return Object.fromEntries(
    SHORTCUT_ACTIONS.map((action) => [action, [...DEFAULT_SHORTCUTS[action]]]),
  ) as ShortcutBindings;
}

function loadBindings(): ShortcutBindings {
  if (typeof localStorage === "undefined") return cloneDefaults();
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}") as Partial<
      Record<ShortcutAction, unknown>
    >;
    const next = cloneDefaults();
    const seen = new Set<string>();
    for (const action of SHORTCUT_ACTIONS) {
      const stored = parsed[action];
      if (!Array.isArray(stored)) {
        next[action] = next[action].filter((binding) => {
          if (seen.has(binding)) return false;
          seen.add(binding);
          return true;
        });
        continue;
      }
      next[action] = stored
        .filter((binding): binding is string => typeof binding === "string" && binding.length > 0)
        .filter((binding) => {
          if (seen.has(binding)) return false;
          seen.add(binding);
          return true;
        });
    }
    return next;
  } catch {
    return cloneDefaults();
  }
}

function persist(bindings: ShortcutBindings) {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(bindings));
}

function normalizeKey(event: KeyboardEvent): string | null {
  if (PURE_MODIFIERS.has(event.key)) return null;
  if (event.key === " ") return "Space";
  if (event.key.length === 1) return event.key.toUpperCase();
  return event.key;
}

export function shortcutBindingFromEvent(event: KeyboardEvent): string | null {
  const key = normalizeKey(event);
  if (!key) return null;
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push("Mod");
  if (event.altKey) parts.push("Alt");
  const printableSymbol = event.key.length === 1 && !/[a-z0-9]/i.test(event.key);
  if (event.shiftKey && !printableSymbol) parts.push("Shift");
  parts.push(key);
  return parts.join("+");
}

export function formatShortcutBinding(binding: string): string {
  const isMac =
    typeof navigator !== "undefined" &&
    /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  return binding
    .split("+")
    .map((part) => (part === "Mod" ? (isMac ? "⌘" : "Ctrl") : part))
    .join("+");
}

export function isReservedShortcut(binding: string): boolean {
  return RESERVED.has(binding) || binding === "Escape" || binding === "Tab" || binding === "Enter";
}

type AddShortcutResult =
  | { kind: "ok" }
  | { kind: "reserved"; binding: string }
  | { kind: "conflict"; binding: string; action: ShortcutAction };

class ShortcutSettings {
  bindings = $state<ShortcutBindings>(loadBindings());

  actionForEvent(event: KeyboardEvent): ShortcutAction | null {
    const binding = shortcutBindingFromEvent(event);
    if (!binding) return null;
    return (
      SHORTCUT_ACTIONS.find((action) => this.bindings[action].includes(binding)) ?? null
    );
  }

  addBinding(action: ShortcutAction, binding: string): AddShortcutResult {
    if (isReservedShortcut(binding)) return { kind: "reserved", binding };
    for (const candidate of SHORTCUT_ACTIONS) {
      if (candidate !== action && this.bindings[candidate].includes(binding)) {
        return { kind: "conflict", binding, action: candidate };
      }
    }
    if (this.bindings[action].includes(binding)) return { kind: "ok" };
    this.bindings = {
      ...this.bindings,
      [action]: [...this.bindings[action], binding],
    };
    persist(this.bindings);
    return { kind: "ok" };
  }

  removeBinding(action: ShortcutAction, binding: string) {
    this.bindings = {
      ...this.bindings,
      [action]: this.bindings[action].filter((candidate) => candidate !== binding),
    };
    persist(this.bindings);
  }

  resetAction(action: ShortcutAction) {
    const defaults = new Set(DEFAULT_SHORTCUTS[action]);
    this.bindings = Object.fromEntries(
      SHORTCUT_ACTIONS.map((candidate) => [
        candidate,
        candidate === action
          ? [...DEFAULT_SHORTCUTS[action]]
          : this.bindings[candidate].filter((binding) => !defaults.has(binding)),
      ]),
    ) as ShortcutBindings;
    persist(this.bindings);
  }

  resetAll() {
    this.bindings = cloneDefaults();
    persist(this.bindings);
  }
}

export const shortcutSettings = new ShortcutSettings();
