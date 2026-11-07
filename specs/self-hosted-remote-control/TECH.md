# Self-hosted remote control — tech

Product spec: [PRODUCT.md](PRODUCT.md).

## Key observation
Only the *server* and *web viewer* of Warp session sharing are proprietary. The wire protocol is
the public, AGPL `session-sharing-protocol` crate (a direct dependency here), and the
sharer/viewer client code is in `synthet/warp`. So the replacement is a server that implements the
protocol plus small client gates — not a new sharing stack.

## Server

| File | Role |
|------|------|
| `src/hub.rs` | Transport-agnostic state machine: sessions, participants, ordered event log, relay rules. Unit-tested without sockets (`hub_tests.rs`). |
| `src/server.rs` | axum routes + websocket plumbing: one writer task per socket fed by an unbounded channel; the hub owns the sender, so dropping it closes the socket. |
| `src/auth.rs` | Shared token via `Authorization: Bearer` or `synth_rc_token` cookie, constant-time compare, same-origin check for upgrades. |
| `src/web.rs`, `web/` | Embedded PWA (`include_bytes!`), vendored xterm.js 5.5.0 + fit addon (MIT). |
| `src/main.rs` | `synth-remote-control-server` CLI (clap, `SYNTH_RC_*` env). |
| `tests/websocket_e2e.rs` | Real TCP: sharer + viewer exchange, dashboard API, auth/origin refusals. |

Routes match what the client already dials via `connect_endpoint`:
`/sessions/create`, `/sessions/{id}/resume`, `/sessions/join/{id}` (websockets),
`/api/sessions`, `/api/login`, `/api/logout`, `/healthz`, `/`, `/session/{id}`, `/assets/*`.

### Protocol behavior
- **Create**: first frame must be `Initialize`; reply `SessionInitialized` with a fresh secret and
  reconnect token. The sharer and every viewer get the same participant uid (`synth-owner`), which
  satisfies the client's same-user rule for viewer-driven sizing (see `specs/APP-7376` in `synthet/warp`).
- **Events**: `OrderedTerminalEvent`s are appended, broadcast, and acked
  (`EventsProcessedAck`). Event numbers at or below the latest are acked but dropped — the sharer
  replays after `Reconnect`. Scrollback + event bytes are capped per session
  (`--max-session-mib`, default 256); overflow sends `SessionTerminated::ExceededSizeLimit`.
- **Resume**: `Reconnect` must match secret and reconnect token. A connection id per socket stops a
  stale socket's disconnect from detaching the resumed one. Sessions with no sharer for
  `--reconnect-grace-minutes` are reaped.
- **Join / rejoin**: `JoinedSuccessfully` (initial scrollback, latest prompt, merged UDI context)
  followed by every stored event; a rejoin with a known `viewer_id` gets `RejoinedSuccessfully`
  and only events after `last_received_event_no`.
- **Viewer → sharer**: `ExecuteCommand`, `WriteToPty`, `SendAgentPrompt`, `SendControlAction`,
  `UpdateInput` require an executing role; `WriteToPty` ids are rewritten to the sending viewer so
  one viewer cannot act as another (the sharer checks the role of the id's participant).
  `ReportTerminalSize`, selections, role requests are relayed.
- **Roles**: new viewers get `--viewer-role` (default `full`, downgraded to `executor` for clients
  without `supports_full_role_for_real`). Sharer role changes are applied and broadcast.
- **ACL editing** (link/team access, guests): answered with protocol error responses — access is
  the token.

## Web app (`web/app.js`)
Speaks the viewer protocol directly (serde's externally tagged JSON). Scrollback blocks are the
client's JSON `SerializedBlock`; the app writes `stylized_command` / `stylized_output` with
bare `\n` normalized to CRLF. Live `PtyBytesRead` payloads are LZ4 blocks with a little-endian
u32 size prefix (`lz4_flex::block::compress_prepend_size`); `app.js` decodes them before writing to
xterm, which also swallows Warp's DCS shell-integration hooks. Input:
- Idle (`CommandExecutionFinished` seen): command bar → `ExecuteCommand { buffer_id = current
  block id }`; the block id comes from `init_block_id` / `next_block_id`.
- Running (`CommandExecutionStarted`): keystrokes, key bar, and command bar → `WriteToPty`.
- Size: `FitAddon.proposeDimensions()` → `ReportTerminalSize`; the sharer's `Resize` events stay
  authoritative for xterm's size.

## Client changes in `synthet/warp` (all marked `Synth Warp is local-first`)

| Where | Change |
|-------|--------|
| `app/src/bin/oss.rs` | `SYNTH_WARP_REMOTE_CONTROL_URL` → `override_session_sharing_server_url`, refusing Warp hosts. Runs before `init_feature_flags`. |
| `warp_core/src/channel/state.rs` | `self_hosted_remote_control_enabled[_for]` (OSS + non-Warp URL). After the subtractive OSS filter, re-adds `CreatingSharedSessions`, `HOARemoteControl`, `SharedSessionWriteToLongRunningCommands`. |
| `shared_session/self_hosted.rs` (new) | Token/host headers, share-all flag, web link. |
| `shared_session/{sharer,viewer}/network.rs` | Append `self_hosted::connect_headers()` at the three `connect_with_headers` calls. |
| `shared_session/mod.rs` `join_link` | Self-hosted web link first. |
| `pane_group/mod.rs` | Share-session entry point skips the Warp login gate and ACL modal. |
| `agent_input_footer/mod.rs` | Remote-control chip not login-gated when self-hosted. |
| `view/shared_session/view_impl.rs` | Self-hosted shares are remote-control shares (banner copy, no ACL dialog) and skip the inactivity revoke/end timer, as ambient-agent shares already do. |
| `terminal/view.rs` | Share-all: share on the first bootstrap only, when `NotShared`. |
| `local_tty/terminal_view_adaptor.rs` | Share-all: when a resume is rejected (server restarted, session reaped), start a fresh share instead of showing an error toast. |
| `workspace/view.rs` | Share-all: don't copy a link per new terminal, and don't confirm closing a shared tab/pane (every tab is shared). |

Upstream code paths are kept and gated, not deleted, per
[cloud-surface-audit.md](https://github.com/synthet/warp/blob/master/docs/architecture/cloud-surface-audit.md#conventions-when-stripping-more).

## Known limits / follow-ups
- A terminal shared after its command started reports idle, because `command_running` is only
  set by `CommandExecutionStarted`. See the
  [verification report](../../docs/reports/remote-control-verification-2026-11-05.md).
- `pwd` is read once from the scrollback at share time, so it stays blank for a terminal shared
  with no finished blocks.
- Settings UI for the env vars (today: environment only).
- The client caps a share at 100 MB (`max_session_size` default); long-lived auto-shared
  terminals end sharing when they exceed it and must be re-shared.
- Warp prompt rendering (`ActivePrompt::WarpPrompt`) and agent conversation events are not
  rendered by the web app.
- No persistence across server restarts; terminals reconnect and get `SessionNotFound`. In
  share-all mode they re-share automatically (new session ids); otherwise the user re-shares.
- The web app renders the PTY stream (LZ4-framed per event, decoded in `app.js`), not Warp's block
  UI, so Warp's own prompt (PS1 is rendered natively by Warp) does not appear in the browser.
