/**
 * Theme handling: the accent color.
 *
 * The app is AMOLED dark only — there is no light mode, so nothing here sets a
 * mode or a `data-theme` attribute. Persisted in localStorage
 * ("jellysic.theme"). An inline script in app.html re-applies the stored
 * accent before first paint, so this module only has to handle changes made
 * from the settings UI; the module-scope init below is a safety net
 * (idempotent) for anything the pre-paint script missed.
 */

/** "cover": the accent follows the playing cover; the stored fixed accent
 *  stays as the fallback (grey artwork, nothing playing yet). */
type AccentMode = "fixed" | "cover";

export interface ThemeSettings {
  accent: string;
  accentHover: string;
  accentMode: AccentMode;
}

/** Hue (0–360) and saturation (0–1) of a cover's most prominent clearly
 *  colored area — see `ambient.ts`. */
export interface CoverHue {
  h: number;
  s: number;
}

interface AccentColors {
  accent: string;
  hover: string;
  /** Text/icons on accent-colored surfaces. */
  onAccent: string;
}

/** Dispatched on `window` whenever the accent variables change, for anything
 *  that caches the color (canvas drawing). */
export const ACCENT_CHANGED = "jellysic:accent";

export interface AccentOption {
  id: string;
  accent: string;
  /** Precomputed ~10% lightened accent (white darkens instead). */
  accentHover: string;
}

export const ACCENTS: AccentOption[] = [
  { id: "green", accent: "#1db954", accentHover: "#1ed760" },
  { id: "blue", accent: "#3b82f6", accentHover: "#4f8ff7" },
  { id: "purple", accent: "#8b5cf6", accentHover: "#976cf7" },
  { id: "pink", accent: "#ec4899", accentHover: "#ee5aa3" },
  { id: "red", accent: "#ef4444", accentHover: "#f15757" },
  { id: "orange", accent: "#f97316", accentHover: "#fa812d" },
  { id: "teal", accent: "#14b8a6", accentHover: "#2cbfaf" },
  { id: "white", accent: "#ffffff", accentHover: "#e6e6e6" },
];

/** Lighten a #rrggbb color toward white (for a hover shade of a custom accent). */
export function lightenHex(hex: string, amount = 0.12): string {
  const h = hex.replace("#", "");
  if (h.length !== 6) return hex;
  const num = parseInt(h, 16);
  if (Number.isNaN(num)) return hex;
  const r = Math.min(255, Math.round(((num >> 16) & 0xff) + 255 * amount));
  const g = Math.min(255, Math.round(((num >> 8) & 0xff) + 255 * amount));
  const b = Math.min(255, Math.round((num & 0xff) + 255 * amount));
  return `#${((r << 16) | (g << 8) | b).toString(16).padStart(6, "0")}`;
}

/** The cover accent is the default: on black, the playing cover is the only
 *  thing that should tint the chrome. The fixed accent stays the fallback. */
export const DEFAULT_THEME: ThemeSettings = {
  accent: ACCENTS[0].accent,
  accentHover: ACCENTS[0].accentHover,
  accentMode: "cover",
};

const STORAGE_KEY = "jellysic.theme";
/** Last cover accent, so the pre-paint script in app.html can start with it
 *  instead of flashing the fixed accent until the cover is analysed. The
 *  version suffix retires the entries of the light/dark era: those were
 *  measured against a different background. */
const COVER_ACCENT_KEY = "jellysic.coverAccent.v2";

export function loadTheme(): ThemeSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_THEME };
    const parsed = JSON.parse(raw) as Partial<ThemeSettings>;
    // A `mode` left over from the light/dark era is ignored — that is the
    // whole migration. An explicit "fixed" is kept, so nobody's deliberate
    // choice flips just because the default moved to "cover".
    return {
      accent: typeof parsed.accent === "string" ? parsed.accent : DEFAULT_THEME.accent,
      accentHover:
        typeof parsed.accentHover === "string"
          ? parsed.accentHover
          : DEFAULT_THEME.accentHover,
      accentMode: parsed.accentMode === "fixed" ? "fixed" : "cover",
    };
  } catch {
    return { ...DEFAULT_THEME };
  }
}

// --- color math (WCAG 2 relative luminance / contrast) ----------------------

function channelLuminance(c: number): number {
  const v = c / 255;
  return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
}

function luminance([r, g, b]: [number, number, number]): number {
  return 0.2126 * channelLuminance(r) + 0.7152 * channelLuminance(g) + 0.0722 * channelLuminance(b);
}

export function contrastRatio(a: number, b: number): number {
  const [hi, lo] = a > b ? [a, b] : [b, a];
  return (hi + 0.05) / (lo + 0.05);
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  const k = (n: number) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [Math.round(f(0) * 255), Math.round(f(8) * 255), Math.round(f(4) * 255)];
}

function toHex([r, g, b]: [number, number, number]): string {
  return `#${((r << 16) | (g << 8) | b).toString(16).padStart(6, "0")}`;
}

function parseHex(hex: string): [number, number, number] | null {
  let h = hex.trim().replace("#", "");
  // The production CSS is minified, so a token written as #000000 in app.css
  // comes back from getComputedStyle as #000. Expand the shorthand.
  if (/^[0-9a-f]{3}$/i.test(h)) h = h.replace(/./g, (c) => c + c);
  if (!/^[0-9a-f]{6}$/i.test(h)) return null;
  const n = parseInt(h, 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

/** WCAG relative luminance of a `#rrggbb` color, or null for anything else. */
export function hexLuminance(hex: string): number | null {
  const rgb = parseHex(hex);
  return rgb ? luminance(rgb) : null;
}

/** WCAG "non-text contrast" — the floor for accent-colored icons and text. */
const MIN_ACCENT_CONTRAST = 3;

/**
 * Text and icons drawn ON an accent surface: black or white, whichever
 * contrasts more. A fixed accent needs this as much as a cover-derived one —
 * a dark custom color with black icons on it hides the play button entirely.
 * Kept in sync with the pre-paint script in app.html.
 */
function onAccentFor(accent: string): string {
  const lum = hexLuminance(accent);
  if (lum === null) return "#000000";
  return contrastRatio(lum, 0) >= contrastRatio(lum, 1) ? "#000000" : "#ffffff";
}

/**
 * The accent for a cover color: its hue, a saturation clamped to something
 * that reads as an accent, and a lightness walked up from the background until
 * accent-colored icons and text stand out against it. Text on accent surfaces
 * takes whichever of black/white contrasts more.
 */
export function coverAccent(cover: CoverHue, baseLuminance: number): AccentColors {
  const s = Math.min(0.85, Math.max(0.45, cover.s));
  let l = 0.6;
  for (let i = 0; i < 20; i++) {
    if (contrastRatio(luminance(hslToRgb(cover.h, s, l)), baseLuminance) >= MIN_ACCENT_CONTRAST) break;
    l = Math.min(0.9, Math.max(0.1, l + 0.02));
  }
  const rgb = hslToRgb(cover.h, s, l);
  const hoverL = Math.min(0.9, Math.max(0.1, l + 0.06));
  return {
    accent: toHex(rgb),
    hover: toHex(hslToRgb(cover.h, s, hoverL)),
    onAccent: onAccentFor(toHex(rgb)),
  };
}

// --- applying ----------------------------------------------------------------

/** The playing cover's color, kept so the accent can be re-derived when the
 *  accent setting changes. */
let coverHue: CoverHue | null = null;

function baseLuminance(): number {
  const token = getComputedStyle(document.documentElement).getPropertyValue("--color-base");
  const rgb = parseHex(token);
  if (rgb) return luminance(rgb);
  // Mirrors --color-base in app.css, for a style sheet that is not loaded yet.
  return luminance([0x00, 0x00, 0x00]);
}

/** Whether the root layout has reported a cover yet (a hue or `null`). */
let coverKnown = false;

/** The last cover accent, as the pre-paint script in app.html reads it. */
function storedCoverAccent(): AccentColors | null {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(COVER_ACCENT_KEY) ?? "null");
    if (!stored || typeof stored !== "object") return null;
    const { accent, hover, onAccent } = stored as Record<string, unknown>;
    if (typeof accent !== "string" || typeof hover !== "string" || typeof onAccent !== "string") {
      return null;
    }
    return { accent, hover, onAccent };
  } catch {
    return null;
  }
}

function accentFor(theme: ThemeSettings): AccentColors {
  if (theme.accentMode === "cover" && coverHue) {
    return coverAccent(coverHue, baseLuminance());
  }
  // Before the first cover is analysed, keep the accent the pre-paint script
  // already applied instead of flashing the fixed one.
  if (theme.accentMode === "cover" && !coverKnown) {
    const stored = storedCoverAccent();
    if (stored) return stored;
  }
  return {
    accent: theme.accent,
    hover: theme.accentHover,
    onAccent: onAccentFor(theme.accent),
  };
}

function writeAccent(theme: ThemeSettings): void {
  const colors = accentFor(theme);
  const root = document.documentElement;
  root.style.setProperty("--color-accent", colors.accent);
  root.style.setProperty("--color-accent-hover", colors.hover);
  root.style.setProperty("--color-on-accent", colors.onAccent);
  if (theme.accentMode === "cover" && coverHue) {
    localStorage.setItem(COVER_ACCENT_KEY, JSON.stringify(colors));
  }
  window.dispatchEvent(new Event(ACCENT_CHANGED));
}

/** Sets the accent custom properties and persists the settings. */
export function applyTheme(theme: ThemeSettings): void {
  // No background on <html>: the window is frameless and transparent, and
  // .app-root paints the base inside its rounded corners (app.css).
  writeAccent(theme);
  localStorage.setItem(STORAGE_KEY, JSON.stringify(theme));
}

/** The playing cover changed (root layout). Takes effect in "cover" mode;
 *  `null` (a cover without a clear color) falls back to the fixed accent. */
export function setCoverHue(hue: CoverHue | null): void {
  coverHue = hue;
  coverKnown = true;
  const theme = loadTheme();
  if (theme.accentMode === "cover") writeAccent(theme);
}

function initTheme(): void {
  applyTheme(loadTheme());
}

if (typeof window !== "undefined") {
  initTheme();
}
