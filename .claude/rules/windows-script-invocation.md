# Windows script invocation (always on)

On Windows, **never invoke a `.sh` by bare path.** Always name the interpreter:

```powershell
bash "C:/path/to/script.sh"      # correct
& "C:/path/to/script.sh"         # WRONG — ShellExecute
Start-Process "C:/…/script.sh"   # WRONG — ShellExecute
```

PowerShell cannot execute `.sh` natively, so a bare path falls through to `ShellExecute`, which
dispatches by **file association** instead of running the script. That silently turns "run this
script" into "open this file in whatever app is associated" — a GUI app, not a shell.

This is not hypothetical here: it opened each script in VS Code, 21 launches in 8 minutes, each
spawning a transient `Code.exe` that held the VS Code install directory open and blocked its
updater with `os error 5` across 22 consecutive retry runs.

Applies with equal force to hook scripts. On Windows, use the Synth Warp installer
(`scripts/install_claude_warp_hooks_windows.ps1` in `synthet/warp`) instead of the bash
`warp@claude-code-warp` plugin. See
[docs/guides/claude-code-warp-windows-hooks.md](https://github.com/synthet/warp/blob/master/docs/guides/claude-code-warp-windows-hooks.md).

Claude Code plugin hooks under `~/.claude/plugins/cache/**/scripts/*.sh` must be invoked as
`bash "<path>"` when testing by hand; never strip the `bash` prefix when copying a hook
definition into another agent's config.

Same principle for `.py` / `.rb` / `.pl`: name the interpreter, don't rely on the association.

Working if: running a script never opens a GUI application.
