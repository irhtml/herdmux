---
title: Keybindings
description: Every shortcut in the sidebar, the agent search, the worktree spawn modal, and the close-pane modal.
---

## Sidebar

| Key            | Action                                                        |
| -------------- | ------------------------------------------------------------- |
| `prefix + e`   | Toggle sidebar                                                |
| `prefix + E`   | Toggle sidebar in all windows                                 |
| `prefix + a`   | Open the agent finder popup (see `@sidebar_key_find`)         |
| `j` / `Down`   | Move selection down                                           |
| `k` / `Up`     | Move selection up                                             |
| `h` / `Left`   | Previous status filter                                        |
| `l` / `Right`  | Next status filter                                            |
| `r`            | Open repo filter popup                                        |
| `s`            | Toggle between this session's agents and every session's      |
| `/`            | Search agents in every session                                |
| `v`            | Toggle compact and full rows                                  |
| `o`            | Toggle repository groups and most-recent-first order          |
| `p`            | Pin or unpin the selected agent                               |
| `Enter`        | Jump to the selected pane                                     |
| `Tab`          | Cycle status filter                                           |
| `Shift+Tab`    | Switch bottom panel tab (Activity, Git, MRs)                  |
| `Esc`          | Return focus or close the popup                               |
| `?`            | Show keymap (`?` or `Esc` to close; arrows or mouse wheel to scroll) |

## Agent search

Opened with `/` in the sidebar. The agent finder popup (`prefix + a`) opens straight into it.

| Key                          | Action                                         |
| ---------------------------- | ---------------------------------------------- |
| Text keys                    | Type the query                                 |
| `Up` / `Down` / `Ctrl+p` / `Ctrl+n` | Move through the matches                |
| `Enter`                      | Jump to the selected agent and close the search |
| `Backspace`                  | Delete a character                             |
| `Ctrl+w`                     | Delete the last word                           |
| `Ctrl+u`                     | Clear the query                                |
| `Esc` / `Ctrl+c`             | Close the search (the popup closes too)        |

## Repo filter popup

Opened with `r` or by clicking the repo filter button in the sidebar header.

| Key           | Action                                 |
| ------------- | -------------------------------------- |
| `j` / `Down`  | Move selection down                    |
| `k` / `Up`    | Move selection up                      |
| `Enter`       | Confirm — filter the list to that repo |
| `Esc`         | Cancel                                 |

## Notices popup

Opened by clicking the `ⓘ` badge shown when hooks or plugin setup are missing.

| Key   | Action          |
| ----- | --------------- |
| `Esc` | Close the popup |

## Worktree

| Key | Action                                 |
| --- | -------------------------------------- |
| `n` | Spawn a new worktree + agent           |
| `x` | Remove the selected spawn-created pane |

## Spawn worktree modal

Opened with `n` on a repo.

| Key                                | Action                                                                                           |
| ---------------------------------- | ------------------------------------------------------------------------------------------------ |
| Text keys                          | Type the name (used as the branch slug and worktree directory name)                              |
| `↑` / `↓` / `Tab` / `Shift+Tab`    | Move focus between `NAME` / `AGENT` / `MODE` fields                                              |
| `←` / `→`                          | Cycle the value when the agent or mode field has focus                                           |
| `Enter`                            | Create the worktree + current-window split and launch the agent in the background                |
| `Esc`                              | Cancel                                                                                           |

## Close pane modal

Opened with `x` on a spawn-created pane.

| Key             | Action                                                                                                    |
| --------------- | --------------------------------------------------------------------------------------------------------- |
| `y` / `Enter`   | Close the owned pane, remove the git worktree (`--force`), and delete the branch (`git branch -D`)       |
| `c`             | Close the owned pane only, keep the worktree and branch on disk                                          |
| `n` / `Esc`     | Cancel                                                                                                    |

Dedicated worktree windows created by older versions retain window-level cleanup.
