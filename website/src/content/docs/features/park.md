---
title: Park an agent
description: Close an agent pane after a short handoff, and resume the session later from the agent finder.
---

An agent you are done with for now still costs attention while its pane stays open. Parking closes the pane but keeps the session, with a short note on where it stands, so you can pick it up later.

## Parking (`z`)

Select an idle agent in the sidebar and press `z`.

| Key | Action |
| --- | --- |
| `y` / `Enter` | Ask the agent for a handoff, then park it |
| `s` | Park it without asking; the agent's last reply is kept instead |
| `n` / `Esc` | Cancel |

With a handoff, herdmux sends the agent this prompt and waits up to 2 minutes for the answer:

```text
[herdmux park] This session is being paused and will be resumed later.
Do not use any tools. Reply with a handoff under 80 words, in exactly three lines:
Goal: what this session is for
State: where the work stands
Next: the next step when it resumes
```

Answering means one more turn, so the agent reads its context once more. When the agent asks for a permission or takes longer than 2 minutes, herdmux stops the turn with `Esc` and keeps the last reply instead.

Then herdmux saves the record, ends the agent with `/exit` (`/quit` for Codex), and closes the pane. A window with nothing left but a sidebar closes too. The last pane of a tmux session stays open as a shell, so the session and its terminals stay attached.

The handoff prompt never becomes the row's latest prompt, and it is left out of the search history.

### What can't be parked

- an agent that is working, or waiting on you (answer it first)
- an agent with a background shell running, since closing the pane would stop it
- an agent that has not reported a session id yet
- an agent that could not be resumed, such as `claude -p` inside a script

## Resuming

Parked sessions show in the [agent search](/features/agent-pane/#finding-an-agent) and the agent finder (`prefix + a`) under a `parked` header, below the live agents. A search matches the tag, Claude's title, the window, repository, branch, tmux session and path, the goal, the last prompt and every handoff line. The row under the cursor shows the handoff's next step.

```text
/ █                                                        4
  ○ invoice export                               billing 10m
  ○ search ranking                             storefront 1h
parked
  ◌ invoice totals                                billing 5h
    ↳ add the rounding test
  ◌ checkout retries                         checkout-api 1d
```

`Enter` on a parked row opens the session in a new window of its old tmux session (or of the current one, when the old one is gone), in its old directory, with its tag and pin. The record is deleted once the agent reports in. `Ctrl+d` forgets a parked session without resuming it.

## From the command line

```sh
herdmux park <target> [--no-handoff]     # target: %12, session:window.pane, a tag, or a worktree name
herdmux park resume <session_id>
herdmux park forget <session_id>
```

Records live in `${XDG_STATE_HOME:-~/.local/state}/herdmux/parked/`, one JSON file per session.
