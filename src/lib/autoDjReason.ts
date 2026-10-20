import { m } from "#lib/paraglide/messages.js";
import type { AutoDjReason, AutoDjSeedMode } from "#lib/types.js";

/** Each seed kind as a queue row names it. Keyed by every mode, so a new one
 *  fails the type check here instead of reaching a row as "undefined". */
const KIND_LABELS: Record<AutoDjSeedMode, () => string> = {
  track: m.settings_autodj_seed_track,
  artist: m.settings_autodj_seed_artist,
  genre: m.settings_autodj_seed_genre,
  album: m.settings_autodj_seed_album,
  playlist: m.settings_autodj_seed_playlist,
  sonic: m.queue_autodj_kind_sonic,
};

/** The caption under a track Auto-DJ appended: what the mix grew from. */
export function autoDjReasonLabel(reason: AutoDjReason): string {
  // Sonarune's own sound seed travels as kind "track", which older builds
  // can read; `bySound` says what the mix really grew from.
  const mode = reason.bySound ? "sonic" : reason.kind;
  // A kind this build does not know would print as "undefined"; the row
  // still says where the track came from.
  if (!Object.hasOwn(KIND_LABELS, mode)) return m.settings_autodj();
  const kind = KIND_LABELS[mode]();
  return reason.label
    ? m.queue_autodj_reason_detail({ kind, label: reason.label })
    : m.queue_autodj_reason({ kind });
}
