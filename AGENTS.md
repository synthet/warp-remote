# Synth Remote Control Server

Standalone server for Synth Warp session sharing. Start with [README.md](README.md). Design is in
[specs/self-hosted-remote-control](specs/self-hosted-remote-control/TECH.md).

## Commands

```bash
cargo build --release
cargo nextest run --no-fail-fast
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Rules

- The wire format comes from `session-sharing-protocol`, pinned by git rev in `Cargo.toml`. Keep it
  at the same rev as `synthet/warp`, or clients and server can disagree.
- Web assets are compiled in with `include_bytes!`. A new file under `web/` needs a route in
  `src/web.rs`.
- Unit tests live in sibling `*_tests.rs` files.
- Startup shared by both binaries lives in `src/launch.rs`. Keep tray logic that can be tested
  without Windows in `src/tray_support.rs`.
- The tray app needs the manifest in `assets/app.manifest`. Without Common Controls v6 it fails
  to load with `0xC0000139`. It has no console, so a panic is silent unless stderr is redirected.
- `warpdotdev/*` repositories are read-only sources. Never push to them or open PRs against them.

## Safety

- The token can type into every shared terminal. Never log it, commit it or paste it into prompts.
  Secrets go in `.env` or a token file, both kept out of git.
- Treat changes to `src/auth.rs` and the origin check as security changes, and test them.
- Plain `ws://` is only for a trusted LAN or tailnet.

## Skills

- `.claude/skills/threat-modeling-agentic-tools`: for changes to auth or the remote-control surface.
- `.claude/skills/rust-unit-tests`: general guidance applies. Its `warpui`, TUI and app-harness
  sections are about the Warp workspace and do not apply here.
