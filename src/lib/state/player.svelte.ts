import { listen } from "@tauri-apps/api/event";
import { api } from "$lib/api";
import { apiLibrary } from "$lib/api/library";
import { net } from "$lib/state/net.svelte";
import { toast } from "$lib/state/toast.svelte";
import { m } from "$lib/paraglide/messages";
import type {
  AudioOutputFallbackEvent,
  PlayerState,
  QueueSnapshot,
  WaveformEvent,
} from "$lib/types";

/** Waveforms kept in the UI: the current track, the prefetched next one, and
 *  a few recent ones for skipping back. */
const WAVEFORMS_KEPT = 8;

const initial: PlayerState = {
  status: "idle",
  current: null,
  sleepRemainingMs: null,
  stopAfter: "off",
  stopAfterItem: null,
  index: 0,
  queueLen: 0,
  positionMs: 0,
  durationMs: 0,
  volume: 1,
  shuffle: false,
  shuffleMode: "off",
  repeat: "off",
  bufferedMs: null,
};

const REPEAT_CYCLE = ["off", "all", "one"] as const;

class PlayerStore {
  state = $state<PlayerState>(initial);
  queue = $state<QueueSnapshot>({
    tracks: [],
    index: 0,
    order: [],
    canUndo: false,
    playedCount: 0,
    duplicateCount: 0,
  });
  showQueue = $state(false);
  /** Bumped by "jump to the playing track"; the queue panel scrolls to the
   *  current entry when this changes. A counter, not a flag: asking twice in a
   *  row has to be two events, and there is nothing to reset. */
  revealCurrent = $state(0);
  showLyrics = $state(false);
  /** Last playback error, shown as a dismissible banner. */
  error = $state<string | null>(null);
  /** Show the queue and put the playing track in front of the user — from
   *  the palette, the shortcut, or anywhere else that knows where they left
   *  off is not where the music is. */
  jumpToCurrent() {
    this.showQueue = true;
    this.revealCurrent++;
  }

  /** Optimistic favorite overrides keyed by itemId — the queue's `isFavorite`
   *  is only a snapshot, and the 400ms state ticks would otherwise revert a
   *  toggle before the server round-trip returns. */
  favOverrides = $state<Record<string, boolean>>({});
  /** Seek-bar waveforms by item id. Rust analyses a track once it is fully
   *  downloaded and announces it with `player:waveform`. */
  waveforms = $state<Record<string, number[]>>({});

  #initialized = false;
  /** When the last player:state event landed — basis for extrapolation. */
  #lastStateAt = performance.now();
  /** Last track whose waveform was asked for, so a tick does not ask again. */
  #waveformAsked: string | null = null;

  #setWaveform(itemId: string, peaks: number[]) {
    const kept = Object.entries(this.waveforms)
      .filter(([id]) => id !== itemId)
      .slice(-(WAVEFORMS_KEPT - 1));
    this.waveforms = { ...Object.fromEntries(kept), [itemId]: peaks };
  }

  /** A UI that missed the event (reload, a track analysed before this window
   *  existed) asks once per track. */
  #askWaveform(state: PlayerState) {
    const id = state.current?.itemId;
    if (!id || id === this.#waveformAsked || id in this.waveforms) return;
    this.#waveformAsked = id;
    api
      .getWaveform(id)
      .then((peaks) => {
        if (peaks) this.#setWaveform(id, peaks);
      })
      .catch(() => {});
  }

  async init() {
    if (this.#initialized) return;
    this.#initialized = true;

    await listen<PlayerState>("player:state", (event) => {
      this.state = event.payload;
      this.#lastStateAt = performance.now();
      this.#askWaveform(event.payload);
    });
    await listen<WaveformEvent>("player:waveform", (event) => {
      this.#setWaveform(event.payload.itemId, event.payload.peaks);
    });
    await listen<QueueSnapshot>("player:queue", (event) => {
      this.queue = event.payload;
    });
    await listen<string>("player:error", (event) => {
      this.error = event.payload;
      // A stream failure may be the server dropping — verify.
      void net.check();
    });
    await listen<AudioOutputFallbackEvent>("audio:output-fallback", (event) => {
      toast.show(m.audio_output_fallback_toast({ device: event.payload.requestedDevice }), {
        kind: "info",
        ms: 5000,
      });
    });

    // The WebView is suspended while the window is hidden (see
    // src-tauri/src/webview_power.rs), and events emitted during that time are
    // gone for good. Whatever played on without us has to be read back.
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "visible") void this.resync();
      });
    }

    await this.resync();
  }

  /** Read player state and queue back from Rust. Used on start, and again
   *  whenever the window becomes visible after having been suspended. */
  async resync() {
    try {
      this.state = await api.getPlayerState();
      this.#lastStateAt = performance.now();
      this.#askWaveform(this.state);
      this.queue = await api.getQueue();
    } catch (e) {
      console.error("player resync failed", e);
    }
  }

  /** Position extrapolated between the ~400 ms state ticks (lyrics sync). */
  estimatedPositionMs(): number {
    const s = this.state;
    const extra = s.status === "playing" ? performance.now() - this.#lastStateAt : 0;
    const estimated = s.positionMs + extra;
    return s.durationMs > 0 ? Math.min(estimated, s.durationMs) : estimated;
  }

  /** Lyrics and queue panels are mutually exclusive. */
  toggleLyricsPanel = () => {
    this.showLyrics = !this.showLyrics;
    if (this.showLyrics) this.showQueue = false;
  };
  toggleQueuePanel = () => {
    this.showQueue = !this.showQueue;
    if (this.showQueue) this.showLyrics = false;
  };

  /** Fire-and-forget a player/queue command; surface rejections as a banner. */
  run = (promise: Promise<unknown>) => {
    promise
      // A successful action while the banner is up recovers it (real ping).
      .then(() => net.recheckIfOffline())
      .catch((e) => {
        this.error = String(e);
        // A failed action may mean the server dropped — verify quietly.
        void net.check();
      });
  };

  /** Favorite state of the current track, with any optimistic override. */
  get currentIsFavorite(): boolean {
    const cur = this.state.current;
    if (!cur) return false;
    return cur.itemId in this.favOverrides ? this.favOverrides[cur.itemId] : cur.isFavorite;
  }

  /** Toggle the current track's favorite; optimistic, reverts on failure. */
  toggleCurrentFavorite = () => {
    const cur = this.state.current;
    if (!cur) return;
    const next = !this.currentIsFavorite;
    this.favOverrides = { ...this.favOverrides, [cur.itemId]: next };
    apiLibrary.setFavorite(cur.itemId, next).catch((e) => {
      this.favOverrides = { ...this.favOverrides, [cur.itemId]: !next };
      this.error = String(e);
    });
  };

  toggle = () => api.playerToggle();
  next = () => api.playerNext();
  prev = () => api.playerPrev();
  seek = (positionMs: number) => api.playerSeek(Math.max(0, Math.round(positionMs)));
  setVolume = (volume: number) => api.playerSetVolume(Math.min(1, Math.max(0, volume)));
  /** Nudge the volume by a delta (for keyboard shortcuts). */
  nudgeVolume = (delta: number) => this.setVolume(this.state.volume + delta);
  /** Seek relative to the current position (for keyboard shortcuts). */
  seekBy = (deltaMs: number) => this.seek(this.state.positionMs + deltaMs);

  #preMuteVolume = 1;
  /** Toggle mute, restoring the pre-mute level on unmute. */
  toggleMute = () => {
    if (this.state.volume > 0) {
      this.#preMuteVolume = this.state.volume;
      return api.playerSetVolume(0);
    }
    return api.playerSetVolume(this.#preMuteVolume > 0 ? this.#preMuteVolume : 1);
  };
  toggleShuffle = () => {
    const next = this.state.shuffleMode === "off"
      ? "tracks"
      : this.state.shuffleMode === "tracks"
        ? "albums"
        : "off";
    return api.playerSetShuffleMode(next);
  };
  /** Cycle repeat off → all → one → off. */
  cycleRepeat = () => {
    const next = REPEAT_CYCLE[(REPEAT_CYCLE.indexOf(this.state.repeat) + 1) % REPEAT_CYCLE.length];
    return api.playerSetRepeat(next);
  };
  clearQueue = () => api.queueClear();
  removePlayed = () => api.queueRemovePlayed();
  removeDuplicates = () => api.queueRemoveDuplicates();
  undoQueue = () => api.queueUndo();
}

export const player = new PlayerStore();
