// Empty the SvelteKit build output before the static adapter recreates it.
// The repo lives on an SMB share, where the adapter's own rimraf occasionally
// throws ENOTEMPTY (a lingering handle keeps the dir non-empty for a moment).
// fs.rm's retry loop rides that out; once the dir is gone the adapter's rimraf
// is a no-op. No-op when build/ is already absent.
import { rmSync } from "node:fs";

rmSync(new URL("../build/", import.meta.url), {
  recursive: true,
  force: true,
  maxRetries: 12,
  retryDelay: 300,
});
