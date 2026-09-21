import { api } from "$lib/api";
import { swrClear } from "$lib/swr";
import type { ConnectResult, SessionInfo } from "$lib/types";

class SessionStore {
  info = $state<SessionInfo | null>(null);
  restoring = $state(true);

  async restore() {
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
    if (result.status === "connected") this.info = result.session;
    return result;
  }

  async disconnect() {
    await api.disconnect();
    // Never show one account's cached data to the next.
    swrClear();
    this.info = null;
  }
}

export const session = new SessionStore();
