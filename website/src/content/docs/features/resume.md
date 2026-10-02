---
title: Resume after reboot
description: Relaunch agent panes restored by tmux-resurrect with their conversations resumed.
---

[tmux-resurrect](https://github.com/tmux-plugins/tmux-resurrect) (and tmux-continuum) bring back windows, panes and directories after a reboot, but agent panes come back as bare shells. herdmux can relaunch them with their sessions resumed.

## Setup

```tmux
set -g @sidebar_resume on
set -g @plugin 'irhtml/herdmux'                # list before tmux-resurrect / tmux-continuum
set -g @plugin 'tmux-plugins/tmux-resurrect'
set -g @plugin 'tmux-plugins/tmux-continuum'
```

herdmux sets resurrect's `post-save-layout` and `post-restore-all` hooks, but only when they are unset or already its own.

## What gets saved

Every resurrect save records, per agent pane:

- the exact command line the agent was started with
- its session id
- `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `XDG_DATA_HOME` (no other environment)
- the pane's `@pane_desc` tag

## What happens on restore

After a restore herdmux types the matching command into each restored pane, 300 ms apart:

| Agent | Command |
| --- | --- |
| Claude Code | `claude <your flags> --resume <id>` |
| Codex | `codex resume <flags> <id>` |
| OpenCode | `opencode --session <id>` |

It only acts within 10 minutes of a tmux server start, only into shell panes whose position, layout and directory match the snapshot, and never relaunches agents that a script started (for example `claude -p` inside a loop).

## Checking it

```sh
herdmux resume save --dry-run       # what the next save would record
herdmux resume restore --dry-run    # what a restore would type, and what it would skip
```

Each restore is logged to `${XDG_STATE_HOME:-~/.local/state}/herdmux/restore.log`.
