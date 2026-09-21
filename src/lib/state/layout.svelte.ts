export type ViewDensity = "compact" | "comfortable";
export type CardSize = "small" | "medium" | "large";
type LayoutPresetName = "compact" | "comfortable";

interface ViewLayout {
  density: ViewDensity;
  cardSize: CardSize;
  sidebarWidth: number;
}

const CARD_SIZE_PIXELS: Record<CardSize, { min: number; stride: number }> = {
  small: { min: 136, stride: 154 },
  medium: { min: 168, stride: 190 },
  large: { min: 208, stride: 232 },
};

// v2: a single global layout (v1 was a per-view map, which was confusing —
// configured from /settings it silently targeted the settings page). Old v1
// data is ignored; everyone starts from the default.
const STORAGE_KEY = "jellysic.layout.v2";
const DEFAULT_LAYOUT: ViewLayout = {
  density: "comfortable",
  cardSize: "medium",
  sidebarWidth: 240,
};

const PRESETS: Record<LayoutPresetName, ViewLayout> = {
  compact: { density: "compact", cardSize: "small", sidebarWidth: 208 },
  comfortable: { ...DEFAULT_LAYOUT },
};

function cleanLayout(value: unknown): ViewLayout {
  const input = value && typeof value === "object" ? (value as Partial<ViewLayout>) : {};
  const density: ViewDensity = input.density === "compact" ? "compact" : "comfortable";
  const cardSize: CardSize =
    input.cardSize === "small" || input.cardSize === "large" ? input.cardSize : "medium";
  const sidebarWidth =
    typeof input.sidebarWidth === "number"
      ? Math.min(320, Math.max(200, Math.round(input.sidebarWidth)))
      : DEFAULT_LAYOUT.sidebarWidth;
  return { density, cardSize, sidebarWidth };
}

function load(): ViewLayout {
  if (typeof localStorage === "undefined") return { ...DEFAULT_LAYOUT };
  try {
    return cleanLayout(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}"));
  } catch {
    return { ...DEFAULT_LAYOUT };
  }
}

function persist(layout: ViewLayout) {
  if (typeof localStorage !== "undefined") {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(layout));
  }
}

/** One layout applied across the whole app. */
class LayoutPreferences {
  current = $state<ViewLayout>(load());

  get active(): ViewLayout {
    return this.current;
  }

  get cardPixels() {
    return CARD_SIZE_PIXELS[this.current.cardSize];
  }

  update(patch: Partial<ViewLayout>) {
    this.current = cleanLayout({ ...this.current, ...patch });
    persist(this.current);
  }

  applyPreset(preset: LayoutPresetName) {
    this.current = { ...PRESETS[preset] };
    persist(this.current);
  }

  reset() {
    this.current = { ...DEFAULT_LAYOUT };
    persist(this.current);
  }
}

export const layoutPreferences = new LayoutPreferences();
