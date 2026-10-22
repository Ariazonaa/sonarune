import { describe, expect, it } from "vitest";
import { m } from "#lib/paraglide/messages.js";
import { autoDjReasonLabel } from "./autoDjReason";
import type { AutoDjReason, AutoDjSeedMode } from "./types";

describe("autoDjReasonLabel", () => {
  it("names the sound seed instead of printing undefined", () => {
    const label = autoDjReasonLabel({ kind: "sonic", label: "Teardrop" });
    expect(label).toBe(
      m.queue_autodj_reason_detail({ kind: m.queue_autodj_kind_sonic(), label: "Teardrop" }),
    );
    expect(label).not.toContain("undefined");
  });

  it("names a sound seed that travels as a track seed", () => {
    expect(autoDjReasonLabel({ kind: "track", label: "Teardrop", bySound: true })).toBe(
      m.queue_autodj_reason_detail({ kind: m.queue_autodj_kind_sonic(), label: "Teardrop" }),
    );
    expect(autoDjReasonLabel({ kind: "track", label: "Teardrop" })).toBe(
      m.queue_autodj_reason_detail({ kind: m.settings_autodj_seed_track(), label: "Teardrop" }),
    );
  });

  it("names every seed kind", () => {
    const kinds: AutoDjSeedMode[] = ["track", "artist", "genre", "album", "playlist", "sonic"];
    for (const kind of kinds) {
      expect(autoDjReasonLabel({ kind, label: "" })).not.toContain("undefined");
      expect(autoDjReasonLabel({ kind, label: "Seed" })).not.toContain("undefined");
    }
  });

  it("leaves the seed out when there is none", () => {
    expect(autoDjReasonLabel({ kind: "genre", label: "" })).toBe(
      m.queue_autodj_reason({ kind: m.settings_autodj_seed_genre() }),
    );
  });

  it("falls back to plain Auto DJ for a kind it does not know", () => {
    const unknown = { kind: "mood", label: "Rainy" } as unknown as AutoDjReason;
    expect(autoDjReasonLabel(unknown)).toBe(m.settings_autodj());
  });
});
