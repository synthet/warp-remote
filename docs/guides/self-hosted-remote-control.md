---
type: Guide
title: Self-hosted remote control
description: Run synth-remote-control-server and control Synth Warp terminals from a phone or browser, replacing Warp's hosted session sharing.
resource: guides/self-hosted-remote-control.md
tags: [docs, guides, remote-control, session-sharing, self-hosted]
timestamp: 2026-10-17T00:00:00Z
okf_version: 0.1
---

# Self-hosted remote control

Watch and drive your Synth Warp terminals — including a CLI agent waiting for approval — from a
phone or any browser. You run the server; no Warp account or warp.dev service is involved.

Design: [specs/self-hosted-remote-control](../../specs/self-hosted-remote-control/PRODUCT.md).

## 1. Run the server

```bash
cargo build --release -p remote_control_server
export SYNTH_RC_TOKEN="$(openssl rand -hex 32)"   # keep this; terminals and the web app need it
./target/release/synth-remote-control-server
```

If no token is set, the server generates one and prints it at startup (valid for that run only).

| Flag / env | Default | Meaning |
|------------|---------|---------|
| `--bind` / `SYNTH_RC_BIND` | `0.0.0.0:8787` | Listen address; the default covers every local IPv4 address |
| `--token` / `SYNTH_RC_TOKEN` | generated | Shared secret |
| `--token-file` / `SYNTH_RC_TOKEN_FILE` | — | Read the secret from a file |
| `--owner-name` / `SYNTH_RC_OWNER_NAME` | `Owner` | Name in participant lists |
| `--viewer-role` / `SYNTH_RC_VIEWER_ROLE` | `full` | `reader` makes the server watch-only |
| `--max-session-mib` / `SYNTH_RC_MAX_SESSION_MIB` | `256` | Per-terminal memory cap |
| `--reconnect-grace-minutes` | `10` | Keep a session this long while its terminal is offline |

### Windows tray app

`synth-remote-control-tray.exe` runs the same server with no console window. It takes the same
flags and `SYNTH_RC_*` variables as the console server.

- Closing or minimizing the window collapses it to the notification area. The server keeps running.
- Left-click the tray icon to show or hide the window. Right-click it for *Open dashboard*, *Copy
  token* and *Quit*. Only *Quit* stops the server.
- Starting it again brings up the running instance rather than starting a second one.
- If `SYNTH_RC_TOKEN` and `SYNTH_RC_TOKEN_FILE` are both unset, it generates a token once and keeps
  it in `%LOCALAPPDATA%\SynthRemoteControl\token`. Use *Copy token* to set
  `SYNTH_WARP_REMOTE_CONTROL_TOKEN` to the same value.
- The log is in `%LOCALAPPDATA%\SynthRemoteControl\server.log` and is replaced at each start. If
  the server can't start, for example because the port is taken, the window shows the reason.

### TLS

Plain `ws://`/`http://` is only for a trusted LAN or a tailnet. Anywhere else, terminate TLS in
front of the server, which also lets the web app install as a PWA. Start the server with
`--bind 127.0.0.1:8787` so only the proxy can reach it. Caddy:

```caddyfile
rc.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

Tailscale: `tailscale serve --bg 8787` and use the `https://<machine>.<tailnet>.ts.net` name.

## 2. Point Synth Warp at it

Set these before launching Synth Warp (`warp-oss`) on each machine whose terminals you want to reach:

```bash
export SYNTH_WARP_REMOTE_CONTROL_URL=wss://rc.example.com      # ws://host:8787 on a LAN
export SYNTH_WARP_REMOTE_CONTROL_TOKEN=<the server token>
export SYNTH_WARP_REMOTE_CONTROL_ALL_TERMINALS=1              # optional: share every terminal
export SYNTH_WARP_REMOTE_CONTROL_HOST_LABEL="work laptop"     # optional: dashboard name
```

On Windows use `setx` (or System Properties → Environment Variables) and restart Warp.

A URL on a `warp.dev` host is refused: this never re-enables Warp's paid service.

### Sharing a terminal

- **One terminal**: pane menu → *Share session*, the `/remote-control` slash command, or the
  *Remote control* chip in an agent footer. The web link is copied to the clipboard.
- **Every terminal**: with `SYNTH_WARP_REMOTE_CONTROL_ALL_TERMINALS=1`, each new terminal shares
  itself once it starts. Links are not copied; open the dashboard instead.
- *Stop sharing* works as usual. Self-hosted shares are not ended by desktop inactivity.

## 3. Use the web app

Open `https://rc.example.com`, sign in with the token, and pick a terminal.

- **Idle prompt**: type in the command bar (or the terminal) and press *Run*.
- **Running program** (shell job, `vim`, Claude Code, …): typing goes straight to it; the key bar
  sends Esc, Tab, ^C, ^D, arrows, and Enter.
- When the phone is the only viewer, the terminal resizes to fit it.
- *Add to Home Screen* installs it as an app.

## Use it from a phone on your LAN

The web link and QR code come from `SYNTH_WARP_REMOTE_CONTROL_URL`. With `127.0.0.1` there, they open the phone itself, so set the URL to this PC's LAN address. The server already listens on every local address.

The connection is plain HTTP, so anyone who can capture traffic on the network can read the token. That token can type into every shared terminal, so do this only on a trusted network and undo it after testing.

Run this in an **Administrator PowerShell**, not `cmd`, after setting `$ip` to your address (`ipconfig` shows it):

```powershell
$ip = '<lan-ip>'
New-NetFirewallRule -DisplayName "Synth Remote (temp)" -Direction Inbound -Protocol TCP -LocalPort 8787 -LocalAddress $ip -RemoteAddress LocalSubnet -Profile Private -Action Allow
[Environment]::SetEnvironmentVariable('SYNTH_WARP_REMOTE_CONTROL_URL',"ws://${ip}:8787",'User')
```

Then restart both apps, so they read the new values:

1. Stop the server and start it from a new window. It should log `listening on http://0.0.0.0:8787`.
2. Fully close Synth Warp and reopen it from the Start menu. This ends any agent session running inside it.
3. Connect the phone to the same network and scan the new QR code. The PC dashboard moves to the LAN address too, so sign in there again.

The QR code does not include the token, so you enter it on the phone. A password manager is the easiest way to get it there.

To undo it, run this in an Administrator PowerShell, then restart both apps and replace the token:

```powershell
Remove-NetFirewallRule -DisplayName "Synth Remote (temp)"
[Environment]::SetEnvironmentVariable('SYNTH_WARP_REMOTE_CONTROL_URL','ws://127.0.0.1:8787','User')
```

Installing the app with *Add to Home Screen* needs HTTPS; see [TLS](#tls).

## Troubleshooting

| Symptom | Check |
|---------|-------|
| No sharing entries in Synth Warp | `SYNTH_WARP_REMOTE_CONTROL_URL` set in the environment Warp was launched from; it must start with `ws://` or `wss://` |
| "Failed to share" | Server reachable from that machine; `SYNTH_WARP_REMOTE_CONTROL_TOKEN` matches (`warp-oss.log` warns when it is unset) |
| Web app keeps asking for the token | The cookie is `SameSite=Strict`; open the server's own URL, not through another site |
| Terminal stopped appearing after a while | A share ends at 100 MB of output (client cap) or when the server restarts; share it again (with `ALL_TERMINALS=1`, a server restart re-shares automatically) |
| Commands rejected as busy | A command is already running; use the key bar / typing instead |
| `binding 0.0.0.0:8787` fails with os error 10048 | Another app uses the port; pass `--bind` with a free port |
| `.exe` shows "can't run on your PC" or `Access is denied` | The file is 0 bytes. In `cmd`, pasting the prompt with the command (`C:\Users\name>D:\...\server.exe`) makes `>` overwrite the file. Rebuild with `cargo clean --release -p remote_control_server`, then `cargo build --release -p remote_control_server` |
| Token warning says it is under 16 characters | PowerShell variables pasted into `cmd` are saved literally; set the token from PowerShell |
| QR code or link opens `127.0.0.1` on a phone | The link comes from `SYNTH_WARP_REMOTE_CONTROL_URL`; use a LAN or HTTPS address there |
| Terminal shows `idle` while a program runs, or mouse movement fills the command bar | Known bug when a terminal is shared after its command started; start the program in an already shared terminal. See the [verification report](../reports/remote-control-verification-2026-11-05.md) |
