---
type: Report
title: Self-hosted remote control verification (2026-11-05)
description: First end-to-end run of self-hosted remote control with a real Synth Warp client, the bugs it found, the setup pitfalls, and the Playwright test plan.
resource: reports/remote-control-verification-2026-11-05.md
tags: [docs, reports, remote-control, session-sharing, windows, playwright]
timestamp: 2026-11-05T21:30:00-06:00
okf_version: 0.1
---

# Self-hosted remote control verification (2026-11-05)

First end-to-end run of [self-hosted remote control](../guides/self-hosted-remote-control.md) with a real client, on Windows, against commit `3c0648ee`. The feature merged into `synthet/warp:master` the same day (`2c2d61f6..3c0648ee`).

**Result:** a Synth Warp terminal shares to the server without a Warp login, and live output reaches the browser. Two web app and server bugs make a terminal that was shared mid-command unusable from the browser. Those block the phone workflow and are not fixed yet.

## Acceptance criteria

Criteria are from [the product spec](../../specs/self-hosted-remote-control/PRODUCT.md#acceptance-criteria).

| AC | Status | Evidence |
|----|--------|----------|
| 1. Share without login, dashboard shows host and working directory | Partly verified | Shared without login and host shown. `pwd` is blank when the terminal had no finished blocks at share time. |
| 2. Scrollback, live output, run a command, send keys | Partly verified | Live output seen in the browser. Input from the browser is not verified. |
| 3. `ALL_TERMINALS=1` shares without touching the clipboard | Partly verified | Terminals shared themselves. Clipboard not checked. |
| 4. No URL means sharing stays hidden | Not tested | |
| 5. A Warp-hosted URL is rejected | Not tested | Code path exists in `apply_remote_control_override`. |
| 6. Missing token and cross-origin upgrades are refused | Verified | See [Auth probes](#auth-probes). |

Not run: a real phone, a server restart with shared terminals, and PWA install.

## Checks that ran

- `cargo nextest` for `remote_control_server` and the `warp_core` channel tests: 45 run, 45 passed.
- Release build of `warp-oss` with `release_bundle,gui`: succeeded in 40m 43s with `-Jobs 2`.
- Deploy with `deploy.ps1 -SkipBuild`: exit 0. The installed `warp.exe` hash matches the build and reports `v0.1.0+3c0648ee`.
- CI on `3c0648ee`: `Warp CI` was still queued at last check, so there is no CI result.

### Auth probes

Run against the release server on a spare port.

| Request | Result |
|---------|--------|
| `/healthz`, `/` | 200 |
| `/api/sessions` with no token or a wrong token | 401 |
| `/api/sessions` with the token | 200 |
| Login with a wrong token / the token | 401 / 204 |
| Websocket upgrade with no token | 401 |
| Websocket upgrade with the token, no `Origin` or same origin | 101 |
| Websocket upgrade with the token from another origin | 403 |

The login cookie is `HttpOnly; SameSite=Strict`, and the token did not appear in the server log. These upgrades used the bearer header, so cookie-based websocket login was exercised only through the browser sign-in.

## Bugs found

### A terminal shared mid-command shows `idle` (not fixed)

The server sets `command_running` from `CommandExecutionStarted` events and starts it at `false` ([`hub.rs`](../../src/hub.rs)). A terminal shared after its command began never sends that event, so the server reports it as idle for the whole run.

Observed side by side on one server:

| Terminal | Shared | Server state | Browser |
|----------|--------|--------------|---------|
| Re-shared with Claude Code already running | After the command started | `command_running=false` | `idle`, typing goes to the command bar |
| Shared before its command started | Before the command started | `command_running=true` | Works |

This is the main blocker for phone use: typing and the key bar do not reach a waiting agent.

The web app has a smaller form of the same bug. `onJoined` in [`app.js`](../../web/app.js) always sets `running = false`, so a viewer that joins mid-command starts as idle even when the server knows better.

**Proposed fix:** derive the state when a share starts. `Scrollback` already carries each block as serialized JSON plus `is_alt_screen_active`, and the server already reads `pwd` from it. This probably needs no client change. Which block field marks a block as unfinished is not confirmed yet. Then have the web app use the server's state on join.

### Mouse movement fills the command bar (not fixed)

A program that enables mouse tracking makes xterm.js emit SGR mouse reports through `onData`. In idle mode, `onTerminalData` treats all input as typed text, drops the escape character and keeps the rest, which gives `[<35;15;40M`.

The trigger is the wrong `idle` state above, so a correct state routes these reports to the program. The idle path should still ignore escape sequences. Do not press *Run* with this text in the bar.

### `pwd` is blank for some terminals (not fixed)

The server reads `pwd` once, from the scrollback at share time (`last_scrollback_pwd`). A terminal shared with no finished blocks stays blank. It should update as commands finish.

### `is_agent` is false for CLI agents (not investigated)

Both shared terminals reported `is_agent=false`, including one running Claude Code, so the dashboard's agent state may not work for CLI agents.

### Viewer counts look high (not investigated)

Terminals reported 2 viewers with one terminal view open. The server was listening on loopback only, so this was not an outside viewer.

### The dashboard hint names the wrong variable (not fixed)

The empty dashboard shows `WARP_SESSION_SHARING_SERVER_URL`. The guide and `app/src/bin/oss.rs` use `SYNTH_WARP_REMOTE_CONTROL_URL`, which is the setting used in this run.

### `deploy.ps1` passes an unknown parameter (not fixed)

`deploy.ps1` passes `CargoProfile` to `prepare_bundled_resources.ps1`, which does not accept it. Deploy reports the failure and falls back to copying `resources\bundled`. The README says a fallback like this skips license and schema generation. The deploy still exits 0.

## Setup pitfalls

| Symptom | Cause | Fix |
|---------|-------|-----|
| `binding 127.0.0.1:8787`, os error 10048 | Another app already uses the port | Use `--bind` with a free port, or move the other app. Here a local dashboard was moved. |
| Blue "this app can't run on your PC" dialog, or `Access is denied` | The server `.exe` was 0 bytes | Rebuild with `cargo clean --release -p remote_control_server`, then `cargo build`. Cargo treats the empty file as up to date otherwise. |
| The `.exe` keeps becoming 0 bytes | Pasting `C:\Users\name>D:\...\server.exe` into `cmd` makes `>` a redirect that truncates the file | Paste only the path, never the prompt. |
| `The remote-control token is shorter than 16 characters` | `$t` pasted into `cmd` was saved literally, because `cmd` does not expand PowerShell variables | Run token commands in PowerShell only. |
| Server returns 401 for the new token | The running process has the old value | Restart from a new window so it reads the updated environment. |
| A new `warp-oss.exe` exits at once | `release_bundle` builds forward to a running instance of the same channel and exit | Close the installed Warp, or test a build without `release_bundle`, with a separate `WARP_DATA_PROFILE`. |
| The QR code opens `127.0.0.1` on the phone | The link comes from `SYNTH_WARP_REMOTE_CONTROL_URL` | Set that URL and the server bind address to a LAN address. |

Closing Warp ends every agent session running inside it. Check for running work before deploying or restarting.

## Questions answered during the run

- **Taskbar and desktop icons differ:** the taskbar uses the icon embedded in the running exe, and the desktop shortcut uses `icon.ico`. The installed exe predated the flat icon family (`fc197611`). Deploying the new build fixes this.
- **Native mobile app:** there isn't one, by design. The web app installs as a PWA, which needs HTTPS on a phone. `127.0.0.1` counts as secure, so Chrome offers to install it there.
- **`warp.exe` and `warp-oss.exe`:** the same program. `warp-oss.exe` is the build output, and deploy installs it as `warp.exe`.
- **The network log looks almost empty:** it is in memory, holds 50 items, resets on restart and shows a snapshot until refreshed. It only covers Warp's HTTP client, so websockets and programs in terminals do not appear. Requests to hosted Warp are blocked before logging.
- **Requests to `192.0.2.0:9`:** update checks to `/client_version` and `/client_version/daily`, sent to this fork's deliberately unroutable server address (`DISABLED_HTTP_ROOT`). They carry no token or terminal content. The OSS build has auto-update off but still runs them, so a guard could stop them. Why the daily check repeats was not traced.

## Phone test (not run yet)

Plain LAN access needs the server bound to a LAN address, `SYNTH_WARP_REMOTE_CONTROL_URL` set to match, and an inbound firewall rule limited to the local subnet and private profile.

The connection is plain HTTP, so the token can be captured on the network. With `ALL_TERMINALS=1` and the default `full` role, that token can type into every shared terminal. Use this only on a trusted network, remove the firewall rule afterwards, and replace the token.

The QR code does not include the token, so it has to be entered on the phone.

## Playwright test plan

Run the real server and a real browser, with a scripted fake terminal in place of Warp.

- `examples/fake_sharer.rs`: reuses the protocol types and LZ4 framing, sends scripted events, and records what it receives. Rust is preferred because a Node version would reimplement the protocol and could drift.
- `web-tests/`: Playwright config and specs. Each run starts its own debug server on a spare port with a throwaway token.

| Group | Cases |
|-------|-------|
| Regressions | Shared mid-command shows `running` and typing reaches the PTY. Viewer joins mid-command. Mouse tracking at idle leaves the bar empty and sends nothing. `pwd` is present. |
| Auth | Wrong token, correct token, reload, logout. |
| Dashboard | Host, working directory, size, state, viewer count, updates. |
| Terminal | Scrollback, live output, *Run*, *Send*, every key-bar key. |
| Failure states | Read-only role, rejected input, session end, reconnect after a server restart. |
| Mobile | Key bar reachable, no sideways scroll, phone size reported, manifest and service worker load. |

Order of work:

1. Branch from `master` and file an issue on `synthet/warp`.
2. Build the fake sharer and one smoke test.
3. Add the regression tests and confirm they fail for the right reason.
4. Fix the server state and `app.js`.
5. Add the remaining groups and fix what they find.
6. Add a CI job.

Done when the regression tests go from failing to passing, all groups pass three runs in a row, and CI passes.

To confirm first: whether xterm's text can be read from the page without a test-only hook, and which Playwright version to pin.

This plan does not cover the real Warp client (AC1, AC3 to AC5), real networks or real phones. WebKit was not installed, so phone sizes are Chromium emulation only.

## Open items

1. Implement the Playwright plan and the fixes above.
2. Run the phone test.
3. Test AC4 and AC5 and check the clipboard for AC3.
4. Fix the dashboard hint and the `deploy.ps1` parameter.
5. Confirm `Warp CI` on `3c0648ee`.
6. Optionally stop update checks in the OSS build.
