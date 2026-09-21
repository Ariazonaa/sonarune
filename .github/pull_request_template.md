<!--
Keep this short. The checklist below is what gets asked for anyway, and the
rules it points at are in CONTRIBUTING.md.
-->

## What this changes

<!-- The behaviour, not the diff. If it fixes an issue, "Fixes #123". -->

## How you tested it

<!--
`npm run verify` passing is the floor, not the answer to this question.

If it touches the audio path, say which formats, whether gapless or crossfade
was involved, and whether the session showed up on the Jellyfin dashboard.
If it touches the interface, say which views you looked at.
If it only works against a real server, say so -- the stand-in Jellyfin in
`scripts/devserver` covers most of the app but not everything.
-->

## Checklist

- [ ] `npm run verify` passes (svelte-check at zero warnings, tests, `cargo fmt`,
      `clippy -D warnings`, `cargo test --lib`, production build)
- [ ] No user-facing string is hardcoded; new ones are in `messages/en.json`
      **and** `messages/de.json`, aria-labels included
- [ ] No color literal in a component; colors come from the tokens in `src/app.css`
- [ ] No Jellyfin request and no audio in the WebView; both go through Rust
- [ ] If it is a workaround, the comment says so and says why
