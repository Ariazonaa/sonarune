import { listen } from "@tauri-apps/api/event";
import { api } from "#lib/api.js";
import { dropSharedSearchHistory } from "#lib/searchHistory.js";
import { downloads } from "#lib/state/downloads.svelte.js";
import { library } from "#lib/state/library.svelte.js";
import { net } from "#lib/state/net.svelte.js";
import { player } from "#lib/state/player.svelte.js";
import { playlists } from "#lib/state/playlists.svelte.js";
import { sonic } from "#lib/state/sonic.svelte.js";
import { swrClear } from "#lib/swr.js";
import type { ConnectResult, SessionEnded, SessionInfo } from "#lib/types.js";

class SessionStore {
  info = $state<SessionInfo | null>(null);
  restoring = $state(true);
  /** Why the backend ended the last session (`session:ended`), until the
   *  next sign-in: the setup screen fills in server and account and says why. */
  ended = $state<SessionEnded | null>(null);
  #listening: Promise<unknown> | null = null;

  /** The backend drops a session it cannot continue — a refused password, a
   *  server that moved to https. Without this the shell stayed signed in while
   *  every call failed with "not connected to a server". Registered before the
   *  restore, which can end a session itself. */
  #listen() {
    this.#listening ??= listen<SessionEnded>("session:ended", (event) =>
      this.#ended(event.payload),
    ).catch((e) => console.error("session:ended listener failed", e));
    return this.#listening;
  }

  #ended(ended: SessionEnded) {
    // Rust says it again on every server check while signed out, for a window
    // that was suspended when it first did. Already on the sign-in screen for
    // this very reason: nothing to redo — the form keeps what was typed.
    const known = this.ended;
    const repeated =
      known?.reason === ended.reason &&
      known.serverUrl === ended.serverUrl &&
      known.username === ended.username;
    if (!this.info && repeated) return;
    this.#forgetAccount();
    this.info = null;
    this.ended = ended;
  }

  /** Nothing of the account that was signed in may show for the next one:
   *  not the cache, not the sidebar's playlists, not a sonic path picked from
   *  its tracks, not its error banner. Its downloads are Rust's to drop: at
   *  sign-out, or when another account signs in after a session the backend
   *  ended (`forget_previous_login`). */
  #forgetAccount() {
    swrClear();
    playlists.reset();
    sonic.cancelPath();
    dropSharedSearchHistory();
    // What was known about the old server says nothing about the next one.
    net.reset();
    player.error = null;
    library.lastSyncedAt = null;
  }

  /** `followEnds`: also go back to the sign-in screen when the backend ends
   *  the session — for the main window, which shows that screen. The
   *  projector restores only to fetch lyrics, and nothing restores it again
   *  after the next sign-in: following an end there would leave its lyrics
   *  off until it is reopened. */
  async restore({ followEnds = false }: { followEnds?: boolean } = {}) {
    if (followEnds) await this.#listen();
    try {
      this.info = await api.restoreSession();
    } catch (e) {
      console.error("session restore failed", e);
      this.info = null;
    } finally {
      this.restoring = false;
    }
  }

  async connect(input: {
    serverUrl: string;
    username: string;
    password: string;
    acceptInvalidCerts: boolean;
    trustFingerprint?: string;
  }): Promise<ConnectResult> {
    const result = await api.connect(input);
    if (result.status === "connected") {
      this.ended = null;
      this.info = result.session;
    }
    return result;
  }

  async disconnect() {
    await api.disconnect();
    this.#forgetAccount();
    // The sign-out cancelled them; the panel may stop listening before the
    // empty list arrives.
    downloads.reset();
    this.ended = null;
    this.info = null;
  }
}

export const session = new SessionStore();
