/** Name suffixes that only resolve on the local network. */
const LOCAL_SUFFIXES = [".local", ".lan", ".home", ".internal", ".home.arpa", ".localhost"];

/**
 * The server address as typed, turned into what `connect` takes: an absolute
 * http(s) URL. A host on this machine or the local network may leave out the
 * scheme and gets `http://`, what Jellyfin serves there by default. Any other
 * host has to name it: guessing plain http for a server across the internet
 * would send the password in the clear, and a wrong guess at https fails only
 * with an opaque network error. Null for an address that cannot work.
 */
export function normalizeServerUrl(raw: string): string | null {
  const value = raw.trim();
  if (!value) return null;
  const hasScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(value);
  const candidate = hasScheme ? value : `http://${value}`;
  let url: URL;
  try {
    url = new URL(candidate);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (!url.hostname) return null;
  if (!hasScheme && !isLocalHost(url.hostname)) return null;
  // As typed otherwise: the address keys the stored credentials and pins.
  return candidate;
}

/** Loopback, private and link-local addresses, names without a dot and the
 *  local-only suffixes. 100.64.0.0/10 counts too: Tailscale's addresses. */
export function isLocalHost(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  if (host === "localhost") return true;
  const v4 = /^(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}$/.exec(host);
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])];
    return (
      a === 10 ||
      a === 127 ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254) ||
      (a === 100 && b >= 64 && b <= 127)
    );
  }
  if (host.includes(":")) {
    // IPv6: loopback, unique local (fc00::/7), link-local (fe80::/10).
    return host === "::1" || /^f[cd]/.test(host) || /^fe[89ab]/.test(host);
  }
  return !host.includes(".") || LOCAL_SUFFIXES.some((suffix) => host.endsWith(suffix));
}
