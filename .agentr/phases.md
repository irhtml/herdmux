# Phases

The plan the agentr build loop works from: one phase issue per section below, each specced, cut
into tickets, built, reviewed and verified by the loop.

## Codex panes show at session start

A newly opened Codex pane appears in the sidebar only after its first prompt. The sidebar shows
a pane once hook metadata has set its tmux options (`@pane_agent`, `@pane_status`, `@pane_cwd`),
and for Codex the `SessionStart` hook sets them. That hook is registered with the matcher
`startup|resume` (`src/adapter/codex.rs`), but Codex CLI 0.142.3 starts a fresh TUI session with
the source `cli`, so Codex never runs the hook. The pane stays blank until `UserPromptSubmit`,
whose matcher is empty, sets the metadata.

Done when a Codex session of any start source runs the `SessionStart` hook: the registration has
no source filter, the generated hook config says `"matcher": ""` for it, and the setup tests
expect exactly that. Small enough for one ticket.
