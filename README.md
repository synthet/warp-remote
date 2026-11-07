# Synth Remote Control Server

Self-hosted server and web/mobile viewer for controlling Synth Warp terminals from a phone or
browser. It builds to one binary, `synth-remote-control-server`, with the web app embedded. No
Warp account or warp.dev service is involved.

## Quick start

```bash
cargo build --release
export SYNTH_RC_TOKEN="$(openssl rand -hex 32)"
./target/release/synth-remote-control-server --bind 127.0.0.1:8787
```

A prebuilt Windows binary is in `bin/` (git-ignored). Setup, TLS, LAN use and troubleshooting are
in the [guide](docs/guides/self-hosted-remote-control.md).

## Layout

| Path | Contents |
|------|----------|
| `src/`, `tests/` | Server and its unit and websocket tests |
| `web/` | Embedded PWA, with vendored xterm.js |
| `examples/fake_sharer.rs` | Scripted terminal used by the browser tests |
| `web-tests/` | Playwright tests (see [Known gaps](#known-gaps)) |
| `config/` | Example environment file and Caddyfile |
| `docs/`, `specs/` | Guide, verification report, product and tech specs |
| `.claude/` | Agent skills and rules |

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --no-fail-fast    # or: cargo test
```

## Known gaps

- `web-tests/package.json` still has the literal version `PLAYWRIGHT_VERSION`. Set a real version
  before `npm install`. Build first with `cargo build --bins --examples`.
- The bugs in the [verification report](docs/reports/remote-control-verification-2026-11-05.md)
  are not fixed, including a terminal shared mid-command showing `idle`.

## Provenance

Extracted on 2026-11-07 from `synthet/warp` at `88308f26` (`crates/remote_control_server`), with
that checkout's uncommitted changes:

- `src/main.rs`: default bind is `0.0.0.0:8787`, not `127.0.0.1:8787`.
- The guide, tech spec and report are the working-tree versions.

`examples/`, `web-tests/` and the `lz4_flex` dev-dependency come from uncommitted work in the
`fix/remote-control-web-state` worktree.

The client half stays in `synthet/warp`: `app/src/terminal/shared_session/self_hosted.rs`,
`app/src/bin/oss.rs` and `crates/warp_core/src/channel/state.rs`. Paths under `app/` and
`deploy.ps1` in these docs refer to that repository.

## License

AGPL-3.0-only, see [LICENSE-AGPL](LICENSE-AGPL). Vendored web dependencies are MIT, see
[web/vendor/README.md](web/vendor/README.md).
