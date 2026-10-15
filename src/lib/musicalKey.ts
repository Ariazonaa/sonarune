import type { MusicalKey } from "#lib/types.js";

// Sharps and flats as musicians usually spell them for that key.
const NAMES_EN = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];
// German names: B is B♭, H is B; minor keys are written lower-case.
const NAMES_DE = ["C", "Cis", "D", "Es", "E", "F", "Fis", "G", "As", "A", "B", "H"];

/** "A minor" / "a-Moll", from the analysis' pitch class and mode. */
export function keyName(key: MusicalKey, locale: string): string {
  const tonic = ((key.tonic % 12) + 12) % 12;
  if (locale.startsWith("de")) {
    const name = NAMES_DE[tonic];
    return key.minor ? `${name.toLowerCase()}-Moll` : `${name}-Dur`;
  }
  return `${NAMES_EN[tonic]} ${key.minor ? "minor" : "major"}`;
}
