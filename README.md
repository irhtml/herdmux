<h1 align="center">herdmux</h1>

<p align="center">One tmux sidebar that tracks every Claude Code, Codex, and OpenCode pane across every session and window, and lets the agents drive each other. See status, background shells, prompts, Git state, activity, and worktrees without switching windows.</p>

<p align="center"><img src="website/src/assets/captures/hero.png" alt="herdmux hero" /></p>

<p align="center">
  <a href="https://irhtml.github.io/herdmux/">Documentation</a> ·
  <a href="https://irhtml.github.io/herdmux/getting-started/installation/">Getting Started</a> ·
  <a href="https://irhtml.github.io/herdmux/features/agent-pane/">Features</a>
</p>

herdmux started as a fork of [hiroppy/tmux-agent-sidebar](https://github.com/hiroppy/tmux-agent-sidebar) and is developed independently.

## Features

- **Every pane, one view**: tracks Claude Code, Codex, and OpenCode panes across all tmux sessions and windows
- **Live metadata**: prompts, tool calls, response previews, background shell state, wait reasons, task progress, and subagent trees refresh as the agents work
- **Worktrees, included**: spawn a fresh worktree + agent in a split of the current window and tear it down (pane, worktree, and branch) in one keystroke
- **Desktop notifications**: native alerts when an agent finishes, needs permission, or errors out
- **Agents driving agents**: `agent list|spawn|prompt|wait|read` lets one agent start, prompt and collect replies from the others
- **Resume after reboot**: with tmux-resurrect, restored panes relaunch their agents with the conversation resumed

OpenCode uses a small local plugin bridge instead of per-event hook config. The plugin lives at `.opencode/plugins/herdmux.js` and can be symlinked as a single file into `~/.config/opencode/plugins/` so it coexists with any existing plugins.

## Requirements

- tmux 3.0+
- [TPM](https://github.com/tmux-plugins/tpm) (or the manual install in [Installation](https://irhtml.github.io/herdmux/getting-started/installation/))
- [GitHub CLI](https://cli.github.com/) (optional, required only for PR numbers in the Git tab)

## Quick Start

### 1. Install the plugin

Using [TPM](https://github.com/tmux-plugins/tpm):

```tmux
set -g @plugin 'irhtml/herdmux'
```

Reload tmux (`tmux source ~/.tmux.conf`), then press `prefix + I`. The install wizard downloads a pre-built binary or builds from source.

### 2. Wire up the agent hooks

- **Claude Code**: register the plugin inside Claude Code:

  ```sh
  /plugin marketplace add ~/.tmux/plugins/herdmux
  /plugin install herdmux@irhtml
  ```

- **Codex**: open a Codex pane, press `prefix + e`, click the yellow `ⓘ` badge, copy the setup snippet, paste it into the Codex pane.
- **OpenCode**: symlink just the plugin file so your existing `~/.config/opencode/plugins/` contents stay untouched:

  ```sh
  mkdir -p ~/.config/opencode/plugins
  ln -sf ~/.tmux/plugins/herdmux/.opencode/plugins/herdmux.js \
    ~/.config/opencode/plugins/herdmux.js
  ```

Full walkthroughs: [Claude Code setup](https://irhtml.github.io/herdmux/getting-started/claude-code/) · [Codex setup](https://irhtml.github.io/herdmux/getting-started/codex/) · [OpenCode setup](https://irhtml.github.io/herdmux/getting-started/opencode/)

### 3. Toggle the sidebar

`prefix + e` toggles the sidebar in the current window, `prefix + E` toggles it everywhere.

## Agents driving agents

The `agent` subcommand reads the same hook-maintained state the sidebar shows, so any agent (or script) can coordinate the others:

```sh
HM="$(tmux show -gv @agent_sidebar_bin)"
"$HM" agent list                                   # panes, state (idle/running/blocked), tag, spawning pane, cwd
"$HM" agent spawn --desc reviewer --wait \
  --prompt "Review the diff on this branch"          # new split, focus stays put; prints pane id, then the reply
"$HM" agent prompt %12 "Now fix the first finding"  # refuses busy or permission-blocked panes unless --force
"$HM" agent wait %12 && "$HM" agent read %12       # exit 0 done, 3 blocked, 4 error, 5 gone, 124 timeout
```

Prompts go in as one bracketed paste and are confirmed through the `UserPromptSubmit` hook. `wait` stops after 110 s by default so it fits in a single agent tool call; Claude Code can run a longer `wait --timeout` as a background command and get notified when it exits. The Claude Code plugin ships a `tmux-agents` skill that teaches agents these rules; run `"$HM" agent help` for every flag.

The skill calls the binary by name. Put it on `PATH` (for example `ln -s "$HM" ~/.local/bin/herdmux`) and the read-only commands can be allowed in Claude Code without opening up `spawn` or `prompt`:

```json
"permissions": { "allow": [
  "Bash(herdmux agent list:*)",
  "Bash(herdmux agent wait:*)",
  "Bash(herdmux agent read:*)"
] }
```

## Resume after reboot

With [tmux-resurrect](https://github.com/tmux-plugins/tmux-resurrect) (and optionally tmux-continuum), restored agent panes come back as shells. Opt in to relaunch them with their sessions resumed:

```tmux
set -g @sidebar_resume on
set -g @plugin 'irhtml/herdmux'    # list before tmux-resurrect / tmux-continuum
set -g @plugin 'tmux-plugins/tmux-resurrect'
set -g @plugin 'tmux-plugins/tmux-continuum'
```

Every resurrect save then records each agent pane's command line, session id, `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `XDG_DATA_HOME`, and `@pane_desc` tag. After a restore the plugin types `claude <your flags> --resume <id>`, `codex resume <flags> <id>` or `opencode --session <id>` into each restored pane, 300 ms apart. It only acts within 10 minutes of a tmux server start, only into shell panes whose position, layout and directory match the snapshot, and never relaunches agents that a script started (for example `claude -p` inside a loop). Preview with `"$HM" resume restore --dry-run`; each run is logged to `~/.local/state/herdmux/restore.log`. The plugin only sets resurrect's `post-save-layout` and `post-restore-all` hooks when they are unset.

## Documentation

The [documentation site](https://irhtml.github.io/herdmux/) covers every feature and option:

- [Agent pane breakdown](https://irhtml.github.io/herdmux/features/agent-pane/)
- [Worktree lifecycle](https://irhtml.github.io/herdmux/features/worktree/)
- [Activity log](https://irhtml.github.io/herdmux/features/activity-log/) · [Git tab](https://irhtml.github.io/herdmux/features/git-status/) · [Notifications](https://irhtml.github.io/herdmux/features/notifications/)
- [Agent support matrix](https://irhtml.github.io/herdmux/agents/)
- [Keybindings](https://irhtml.github.io/herdmux/reference/keybindings/) · [tmux options](https://irhtml.github.io/herdmux/reference/tmux-options/) · [Scripting](https://irhtml.github.io/herdmux/reference/scripting/)

## Development

Symlink the plugin directory to your working copy so builds are picked up without copying:

```sh
rm -rf ~/.tmux/plugins/herdmux
ln -s <path-to-this-repo> ~/.tmux/plugins/herdmux
cargo build --release
```

Toggle the sidebar off → on to pick up the new binary.

### Picking up local builds for the Claude Code plugin

If you also installed this as a Claude Code plugin (`/plugin`), its install path
holds a copy of the released binary that hooks resolve before falling back to
`target/release/`. To make local builds flow through Claude Code hooks too,
replace that copy with a symlink to your working copy:

```sh
# Replace the cached plugin install with a symlink to your repo
PLUGIN_CACHE=~/.claude/plugins/cache/irhtml/herdmux/<version>
rm -rf "$PLUGIN_CACHE"
ln -s <path-to-this-repo> "$PLUGIN_CACHE"
```

Also remove the stale release binary at `bin/herdmux` in your repo
if present: both the tmux launcher and `hook.sh` prefer `bin/` over
`target/release/`, so a leftover binary there will mask `cargo build --release`
output:

```sh
rm -f bin/herdmux
```

Note: Claude Code's plugin updater may overwrite the symlink on a future
update; re-run the symlink step if that happens.

## License

[MIT](./LICENSE)
