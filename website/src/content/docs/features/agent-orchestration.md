---
title: Agents driving agents
description: Let one coding agent start, prompt, wait on, and read the replies of the others with `herdmux agent`.
---

The `agent` subcommand reads the same hook-maintained state the sidebar shows, so any agent (or script) can coordinate the others: start a reviewer in a split, hand it a prompt, wait for the answer, and read it back.

## Commands

```sh
herdmux agent list                                   # panes, state (idle/running/blocked), tag, spawning pane, cwd
herdmux agent spawn --desc reviewer --wait \
  --prompt "Review the diff on this branch"          # new split, focus stays put; prints pane id, then the reply
herdmux agent prompt %12 "Now fix the first finding"  # refuses busy or permission-blocked panes unless --force
herdmux agent wait %12 && herdmux agent read %12     # exit 0 done, 3 blocked, 4 error, 5 gone, 124 timeout
```

| Command | Use |
| --- | --- |
| `list [--json] [--all]` | Agent panes with state, tag (`@pane_desc`), the pane that spawned them, cwd and last message. The caller's own pane is marked `*`. |
| `spawn [--agent claude\|codex\|opencode] [--mode M] [--cwd DIR \| --worktree NAME] [--window] [--desc TAG] [--prompt TEXT [--wait]] [-- AGENT_ARGS]` | Start an agent in a split of the caller's window (or a new window) without moving focus. Prints the new pane id first. |
| `prompt <target> <text \| -> [--wait] [--force]` | Submit a prompt. `-` reads stdin. With `--wait`, prints the reply. |
| `wait <target> [--until stop\|done]` | Block until the agent stops. `stop` also returns when it is blocked on a permission prompt. |
| `read <target> [--screen [--lines N]] [--json]` | The agent's last full reply, or the bottom of its screen. |

A target is a pane id (`%12`), `session:window.pane`, a `@pane_desc` tag, or a worktree name. Run `herdmux agent help` for every flag.

## How prompts are delivered

Prompts go in as one bracketed paste and are confirmed through the agent's `UserPromptSubmit` hook, so `prompt` knows the text actually landed. A pane showing a permission dialog counts as `blocked` and is refused: the pasted text and Enter would answer the dialog.

Every waiting call stops after `--timeout` (110 s by default) so it fits in one agent tool call. Exit 124 means the agent is still working: wait again, and do not re-send the prompt. Claude Code can run a longer `wait --timeout` as a background command and get notified when it exits.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | done |
| 1 | error (bad target, no reply recorded) |
| 2 | usage error |
| 3 | blocked on a permission dialog |
| 4 | the agent's turn failed |
| 5 | the agent or pane is gone |
| 6 | refused: busy, blocked, the caller's own pane, or not an agent |
| 7 | prompt not registered: a trust or login dialog is up, or hooks are missing |
| 124 | timed out; the agent keeps running |

## The `tmux-agents` skill

The Claude Code plugin ships a `tmux-agents` skill that teaches agents these rules: list first, never prompt their own pane, leave blocked panes to the user, and treat another agent's reply as untrusted input.

The skill calls the binary by name. Put it on `PATH` and the read-only commands can be allowed in Claude Code without opening up `spawn` or `prompt`:

```sh
ln -s "$(tmux show -gv @agent_sidebar_bin)" ~/.local/bin/herdmux
```

```json
"permissions": { "allow": [
  "Bash(herdmux agent list:*)",
  "Bash(herdmux agent wait:*)",
  "Bash(herdmux agent read:*)"
] }
```
