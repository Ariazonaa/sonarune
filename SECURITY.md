# Security

## Reporting

Use GitHub's private reporting: **Security → Report a vulnerability** on
[the repository](https://github.com/Ariazonaa/jellysic/security/advisories/new).
That channel is private until an advisory is published, so nothing is disclosed
while a fix is being written. Please do not open a public issue for a
vulnerability.

This is a one-person project. Expect a first reply within a week rather than
within a day. If a report is valid and the fix is small, it usually ships in the
next release; if it is not, you will get a reason rather than silence.

## Supported versions

Only the latest release. There are no maintenance branches, and 0.x means the
next version may change things freely.

## What the app already assumes

Some of this is worth knowing before you report something as a finding.

**Your server is trusted, your network is not.** Jellysic talks to a Jellyfin
server you chose. A malicious server can obviously serve malicious metadata, and
that is not a vulnerability in the client. A malicious *network* between you and
the server is what the design defends against.

**All server traffic runs in Rust, never in the WebView.** Sign-in, browsing,
cover art and audio streams go through `reqwest`. Access tokens never reach the
DOM and never land in SQLite; they live in the Windows Credential Manager, keyed
per server and user.

**Certificates are pinned per server.** A custom rustls verifier checks the OS
trust store first and accepts an untrusted leaf only when its SHA-256
fingerprint was confirmed by the user and stored for that server. There is no
blanket "accept invalid certificates" switch, and the auto-trust flag is kept
only while a pin exists — a login that succeeds with nothing pinned proves a
public certificate, and the flag is dropped, so a later silent re-login cannot
pin whatever an attacker presents.

**Visualizer presets are code.** Butterchurn compiles preset equations with
`new Function`, inside a window that can invoke every command. Local preset
packs ship with the app. The online "Butterchurn Weekly" packs are behind an
explicit, persisted opt-in that is off by default: they are downloaded in Rust
from a fixed origin with no redirects and a size cap, and handed to the WebView
only when their SHA-256 matches a manifest pinned in the repository. A preset
that fails that check is never evaluated.

**A Content-Security-Policy is set.** Scripts come from the app only, images
from the app plus `data:`, `blob:` and the internal `jfimg` protocol, and
connections go to IPC and `jfimg` alone. `'unsafe-eval'` is present because
Butterchurn needs it; that is the reason the preset pinning above exists.

**The diagnostics export is redacted** before it is written: URLs, credentials,
absolute paths and long hex ids are dropped, and quoted dynamic values are
replaced. It is meant to be safe to attach to an issue. If you find something
that leaks through it, that is a valid report.

## Not vulnerabilities

- The installer is not code-signed, so SmartScreen warns about an unknown
  publisher. That is a reputation problem, and it is known.
- Pointing the app at a server you control and having it return hostile data.
- Reading your own tokens out of your own Credential Manager as your own user.
- Findings from automated scanners without a demonstrated impact.
