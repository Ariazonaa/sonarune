declare module "butterchurn" {
  export interface ButterchurnVisualizer {
    connectAudio(node: AudioNode): void;
    loadPreset(preset: unknown, blendTime?: number): void;
    setRendererSize(width: number, height: number): void;
    render(): void;
    /** MilkDrop song-title overlay animation. */
    launchSongTitleAnim(text: string): void;
    /** Per-pixel mesh resolution (the main quality/perf knob). */
    setInternalMeshSize(width: number, height: number): void;
    /** Toggle output anti-aliasing. */
    setOutputAA(useAA: boolean): void;
    /** PNG data URL of the current frame. */
    toDataURL(): string;
  }
  const butterchurn: {
    createVisualizer(
      context: AudioContext,
      canvas: HTMLCanvasElement,
      options: { width: number; height: number; pixelRatio?: number },
    ): ButterchurnVisualizer;
  };
  export default butterchurn;
}

declare module "butterchurn-presets" {
  const presets: {
    getPresets(): Record<string, unknown>;
  };
  export default presets;
}

declare module "butterchurn-presets/lib/butterchurnPresetsExtra.min.js" {
  const presets: {
    getPresets(): Record<string, unknown>;
  };
  export default presets;
}

declare module "butterchurn-presets/lib/butterchurnPresetsExtra2.min.js" {
  const presets: {
    getPresets(): Record<string, unknown>;
  };
  export default presets;
}

declare module "butterchurn-presets/lib/butterchurnPresetsMD1.min.js" {
  const presets: {
    getPresets(): Record<string, unknown>;
  };
  export default presets;
}

declare module "butterchurn-presets/lib/butterchurnPresetsMinimal.min.js" {
  const presets: {
    getPresets(): Record<string, unknown>;
  };
  export default presets;
}

declare module "butterchurn-presets/lib/butterchurnPresetsNonMinimal.min.js" {
  const presets: {
    getPresets(): Record<string, unknown>;
  };
  export default presets;
}
