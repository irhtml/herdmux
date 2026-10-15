---
title: Merge requests
description: Open merge requests and pull requests for every branch checked out in the current tmux session.
---

The bottom panel's **MRs** tab lists the open merge requests (GitLab) and pull requests (GitHub) whose source branch is checked out in any pane of the sidebar's tmux session. Click a row to open it in the browser.

```
╭ Activity │ Git │ MRs ───────────────╮
│!128 Add rate limiting to the login e…│
│  storefront · feature/login-rate-lim…│
│!131 Bump the payment SDK             │
│  draft · checkout · deps/payment-sdk │
│#57 Fix dark mode contrast            │
│  docs-site · fix/contrast            │
╰──────────────────────────────────────╯
```
Switch to the tab with `Shift+Tab` or by clicking its label.

## Which MRs appear

Every pane in the session counts, agent or not. For each pane's working directory the sidebar reads the repo's `origin` remote and current branch, then looks up open MRs with that source branch:

- **GitLab** remotes (any host whose name contains `gitlab`) use [`glab`](https://gitlab.com/gitlab-org/cli): `glab mr list --source-branch`
- **GitHub** remotes use [`gh`](https://cli.github.com/): `gh pr list --head`

Both CLIs must be installed and logged in (`glab auth login`, `gh auth login`). Other hosts, detached `HEAD`s, and directories outside a git repo are skipped.

GitLab MR numbers show as `!123`, GitHub PR numbers as `#123`. Drafts are dimmed and marked `draft`.

## Refresh and cost

The tab only polls while it is on screen: once when it comes into view, then every 60 seconds. Results are stored in `~/.cache/herdmux/merge-requests.json` (or under `$XDG_CACHE_HOME`), shared by every sidebar, and each branch is looked up again only after 5 minutes. Opening the tab in another window of the same session shows the cached list right away.

A new MR can take up to 5 minutes to appear.

The panel shows two lines per MR. Scroll with the mouse wheel, or raise `@sidebar_bottom_height` to see more at once.
