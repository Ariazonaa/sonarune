import { listen } from "@tauri-apps/api/event";
import { api } from "#lib/api.js";
import { apiLibrary } from "#lib/api/library.js";
import { net } from "#lib/state/net.svelte.js";
import { session } from "#lib/state/session.svelte.js";
import { toast } from "#lib/state/toast.svelte.js";
import { m } from "#lib/paraglide/messages.js";
import { UnmuteVolume } from "#lib/unmuteVolume.js";
import { stepVolume } from "#lib/volume.js";
import type {
  AudioOutputFallbackEvent,
  PlayerState,
  QueuePosition,
  QueueSnapshot,
  WaveformEvent,
} from "#lib/types.js";

/** Waveforms kept in the UI: the current track, the prefetched next one, and
 *  a few recent ones for skipping back. */
const WAVEFORMS_KEPT = 8;

/** How far the position is carried on past the last state: a couple of
 *  ticks. A state older than that is stale (the WebView was suspended, the
 *  player was busy), and running on from it would carry the lyrics off to
 *  the end of the track. */
const EXTRAPOLATE_MAX_MS = 1000;

const initial: PlayerState = {
  status: "idle",
  pendingPause: false,
  current: null,
  sleepRemainingMs: null,
  stopAfter: "off",
  stopAfterEntry: null,
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
  speed: 1,
  preservePitch: true,
};

const REPEAT_CYCLE = ["off", "all", "one"] as const;

/** The signed-in account, for what belongs to it alone. Item ids are the
 *  server's, the same for every user on it. */
function account(): string {
  const info = session.info;
  return info ? `${info.serverUrl}\n${info.userId}` : "";
}

class PlayerStore {
  state = $state<PlayerState>(initial);
  queue = $state<QueueSnapshot>({
    // No revision Rust ever sends: a position event before the first
    // snapshot reads the queue back.
    revision: -1,
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
  /** Bumped when a device's sound profile replaced the EQ/volume settings,
   *  so an open Settings page reloads them. */
  audioProfileRevision = $state(0);
  /** Show the queue and put the playing track in front of the user — from
   *  the palette, the shortcut, or anywhere else that knows where they left
   *  off is not where the music is. */
  jumpToCurrent() {
    this.showQueue = true;
    // The two panels share one place (see the toggles below).
    this.showLyrics = false;
    this.revealCurrent++;
  }

  /** Track favorites set in this session, keyed by itemId. The queue's
   *  `isFavorite` is a snapshot from when the queue was built, a page's tracks
   *  one from when it loaded, and the 400ms state ticks would revert a toggle
   *  before the server round-trip returns. What was set since wins over all of
   *  them, wherever it was set — every track heart goes through
   *  `setFavorite`. They are one account's: the next one to sign in, on the
   *  same server too, has favorites of its own (see `#favAccount`). */
  favOverrides = $state<Record<string, boolean>>({});
  /** The account `favOverrides` were set under. */
  #favAccount = "";
  /** Seek-bar waveforms by item id. Rust analyses a track once it is fully
   *  downloaded and announces it with `player:waveform`. */
  waveforms = $state<Record<string, number[]>>({});

  #initialized = false;
  /** When the last player:state event landed — basis for extrapolation. */
  #lastStateAt = performance.now();
  /** Last track whose waveform was asked for, so a tick does not ask again. */
  #waveformAsked: string | null = null;
  /** The level unmute returns to, learned from the reported volumes. */
  #unmute = new UnmuteVolume();

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
      this.#unmute.observe(event.payload.volume, this.#lastStateAt);
      this.#askWaveform(event.payload);
    });
    await listen<WaveformEvent>("player:waveform", (event) => {
      this.#setWaveform(event.payload.itemId, event.payload.peaks);
    });
    await listen<QueueSnapshot>("player:queue", (event) => {
      this.queue = event.payload;
    });
    // A track change moves the position only. The fields are set one by one,
    // so the track list keeps its identity and nothing built from it is
    // rebuilt. A position for another revision means a snapshot was missed
    // (a suspended window): then the queue is read back.
    await listen<QueuePosition>("player:queue-index", (event) => {
      const position = event.payload;
      if (position.revision !== this.queue.revision) {
        void this.#reloadQueue();
        return;
      }
      this.queue.index = position.index;
      this.queue.canUndo = position.canUndo;
      this.queue.playedCount = position.playedCount;
      this.queue.duplicateCount = position.duplicateCount;
    });
    await listen<string>("player:error", (event) => {
      this.error = event.payload;
      // A stream failure may be the server dropping — verify.
      void net.check();
    });
    await listen<{ name: string }>("player:stream-broke-off", (event) => {
      this.error = m.player_stream_broke_off({ name: event.payload.name });
      void net.check();
    });
    await listen<{ device: string }>("audio:profile-applied", (event) => {
      // The volume changed with the device. Rust sends this before the state
      // that carries the device's volume, so that one lands on the new device.
      this.#unmute.switchDevice(event.payload.device, performance.now());
      this.audioProfileRevision++;
      toast.show(m.audio_profile_applied_toast({ device: event.payload.device }), {
        kind: "info",
        ms: 4000,
      });
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
      this.#unmute.observe(this.state.volume, this.#lastStateAt);
      // Ask again even for a track asked about before: that answer may have
      // been "not analysed yet", and the event that followed lost with the
      // suspension.
      this.#waveformAsked = null;
      this.#askWaveform(this.state);
      this.queue = await api.getQueue();
    } catch (e) {
      console.error("player resync failed", e);
    }
  }

  async #reloadQueue() {
    try {
      this.queue = await api.getQueue();
    } catch (e) {
      console.error("queue reload failed", e);
    }
  }

  /** Position extrapolated between the ~400 ms state ticks (lyrics sync).
   *  The position is track time, which runs at the playback speed. */
  estimatedPositionMs(): number {
    const s = this.state;
    const elapsed =
      s.status === "playing" ? Math.min(performance.now() - this.#lastStateAt, EXTRAPOLATE_MAX_MS) : 0;
    const estimated = s.positionMs + elapsed * (s.speed > 0 ? s.speed : 1);
    return s.durationMs > 0 ? Math.min(estimated, s.durationMs) : estimated;
  }

  /** Whether Play/Pause stands for "pause": playing, or loading a source that
   *  plays when it arrives. One that arrives paused (Pause pressed while it
   *  loads, a paused track reopened by a seek) offers Play. */
  get playing(): boolean {
    const s = this.state;
    return s.status === "playing" || (s.status === "loading" && !s.pendingPause);
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

  /** A track's favorite state: what this account set in this session,
   *  otherwise the snapshot the caller holds. */
  isFavorite(itemId: string, snapshot: boolean): boolean {
    if (account() !== this.#favAccount) return snapshot;
    return itemId in this.favOverrides ? this.favOverrides[itemId] : snapshot;
  }

  /** Set a track's favorite — the one way to, so every heart for it agrees.
   *  Optimistic: shown at once; reverts and rejects if the server refuses. */
  async setFavorite(itemId: string, favorite: boolean): Promise<void> {
    const owner = account();
    if (owner !== this.#favAccount) {
      // Signed in as someone else since: the old hearts were not theirs.
      this.#favAccount = owner;
      this.favOverrides = {};
    }
    this.favOverrides = { ...this.favOverrides, [itemId]: favorite };
    try {
      await apiLibrary.setFavorite(itemId, favorite);
    } catch (e) {
      if (this.#favAccount === owner) {
        this.favOverrides = { ...this.favOverrides, [itemId]: !favorite };
      }
      throw e;
    }
  }

  /** Favorite state of the current track, with anything set since. */
  get currentIsFavorite(): boolean {
    const cur = this.state.current;
    return cur ? this.isFavorite(cur.itemId, cur.isFavorite) : false;
  }

  /** Toggle the current track's favorite; optimistic, reverts on failure. */
  toggleCurrentFavorite = () => {
    const cur = this.state.current;
    if (!cur) return;
    this.setFavorite(cur.itemId, !this.currentIsFavorite).catch((e) => {
      this.error = String(e);
    });
  };

  toggle = () => api.playerToggle();
  next = () => api.playerNext();
  prev = () => api.playerPrev();
  seek = (positionMs: number) => api.playerSeek(Math.max(0, Math.round(positionMs)));
  setVolume = (volume: number) => api.playerSetVolume(Math.min(1, Math.max(0, volume)));
  /** Nudge the volume by `delta` of slider travel (for keyboard shortcuts),
   *  so a press moves the player bar's slider the same distance at any level. */
  nudgeVolume = (delta: number) => this.setVolume(stepVolume(this.state.volume, delta));
  /** Seek relative to the current position (for keyboard shortcuts). */
  seekBy = (deltaMs: number) => this.seek(this.state.positionMs + deltaMs);

  /** Toggle mute; unmute returns to the last level listened at, also when the
   *  0 came from the slider or a restart (see lib/unmuteVolume.ts). */
  toggleMute = () => {
    if (this.state.volume > 0) {
      this.#unmute.remember(this.state.volume);
      return api.playerSetVolume(0);
    }
    return api.playerSetVolume(this.#unmute.level);
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
