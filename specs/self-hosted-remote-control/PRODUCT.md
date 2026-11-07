# Self-hosted remote control

## Summary
Control any Synth Warp terminal from a phone or another computer through a server you run
yourself. This replaces Warp's hosted "Remote control" / session sharing, a paid warp.dev
service that Synth Warp strips. No Warp account is involved.

## Problem
Upstream Warp shares a terminal by streaming it to `wss://sessions.app.warp.dev` and viewing it
in Warp's closed web app. Synth Warp ships `WarpServerConfig::disabled()`, so the sharer code is
present but has no server, and every entry point is hidden behind a Warp login. Users who leave a
long task running (a build, a CLI agent such as Claude Code waiting for approval) have no way to
watch or answer it away from the desk.

## Goals
1. A single self-contained server binary (`synth-remote-control-server`) the user runs on a LAN,
   tailnet, or VPS. One shared token authorizes terminals and viewers.
2. A web/mobile app served by that binary: a dashboard of **all** shared terminals across
   machines, and a terminal view that can watch, run commands, and type into running programs.
3. Synth Warp terminals can be shared to it one at a time (pane menu "Share session",
   `/remote-control`, agent footer "Remote control") or **all automatically**.
4. Nothing is sent to Warp Inc.; Warp's hosts are refused as the server URL.

## Non-goals
- Multi-user accounts, per-session ACLs, guests, or teams. The token holder is the owner.
- Warp-agent prompts from the web app (`SendAgentPrompt`): the built-in agent is inert in OSS
  builds. CLI agents are driven as ordinary running programs.
- Persisting sessions across server restarts. Sessions live in memory.
- A native mobile app. The web app installs as a PWA.

## User experience

### Setup
```bash
# Server (any machine the phone can reach)
SYNTH_RC_TOKEN="$(openssl rand -hex 32)" synth-remote-control-server --bind 0.0.0.0:8787

# Each Synth Warp machine
export SYNTH_WARP_REMOTE_CONTROL_URL=wss://rc.example.com
export SYNTH_WARP_REMOTE_CONTROL_TOKEN=<same token>
export SYNTH_WARP_REMOTE_CONTROL_ALL_TERMINALS=1   # optional
```

### In Synth Warp
- With the URL set, sharing entry points appear without a login prompt. Starting a share skips
  the Warp ACL dialog, inserts the inline "Remote control active" banner, and copies the web link
  (`https://rc.example.com/session/<id>`) with a "Remote control link copied." toast.
- With `ALL_TERMINALS=1`, every new terminal is shared once on its first bootstrap. The link is not
  copied (the dashboard lists them), so the clipboard is left alone, and closing a tab does not
  ask to confirm ending its share. A terminal the user stops sharing is not re-shared by a later
  subshell. If the server restarts, auto-shared terminals share themselves again.
- Self-hosted shares do not revoke control or end after local inactivity: the point is driving an
  unattended terminal. Stopping sharing works as upstream.

### In the web app
- Sign in once with the token (stored as an `HttpOnly`, `SameSite=Strict` cookie).
- Dashboard: one card per terminal with machine name, working directory, size, idle/running/agent
  state, viewer count, and last activity; refreshes every few seconds.
- Terminal view: scrollback, then live output. At an idle prompt the command bar runs a command.
  While a program runs, typing and the key bar (Esc, Tab, ^C, ^D, arrows, Enter) go straight to
  it. The viewer reports its size so a phone gets a phone-sized PTY when it is the only viewer.
- Banners for read-only, rejected input, and session end; automatic reconnect with catch-up.

## Security
- Every websocket and API call requires the token (bearer header for terminals, cookie for the
  browser). Comparison is constant-time; failed logins are delayed.
- Cross-origin websocket upgrades are refused, so another site cannot ride the cookie.
- The token is only attached when the configured server is self-hosted (never a Warp host).
- Plain `ws://` is for trusted networks; use TLS (reverse proxy) anywhere else.
- `--viewer-role reader` makes the server watch-only.

## Acceptance criteria
1. With the server running and the env set, a Synth Warp OSS build shares a terminal without a
   Warp login, and the session appears on the dashboard with its host and working directory.
2. The web app replays scrollback and live output, runs a command at an idle prompt, and sends
   keystrokes to a running program.
3. `SYNTH_WARP_REMOTE_CONTROL_ALL_TERMINALS=1` shares each new terminal without touching the
   clipboard.
4. Without `SYNTH_WARP_REMOTE_CONTROL_URL`, OSS behavior is unchanged: sharing stays hidden.
5. A Warp-hosted URL in `SYNTH_WARP_REMOTE_CONTROL_URL` is rejected.
6. Requests without the token, or cross-origin upgrades, are refused.
