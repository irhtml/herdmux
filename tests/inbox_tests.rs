//! Frames for unread replies (`@pane_unread`) and the inbox filter.

#[allow(dead_code, unused_imports)]
mod test_helpers;

use herdmux::state::{StatusFilter, ViewMode};
use herdmux::tmux::{AgentType, PaneInfo, PaneStatus};
use test_helpers::*;

fn agent(id: &str, status: PaneStatus, tag: &str, secs_ago: u64) -> PaneInfo {
    let mut p = make_pane(AgentType::Claude, status);
    p.pane_id = id.into();
    p.pane_active = false;
    p.pane_desc = tag.into();
    p.activity_at = Some(FIXED_NOW - secs_ago);
    p
}

fn unread(mut pane: PaneInfo) -> PaneInfo {
    pane.unread = true;
    pane
}

/// Agent list only: no bottom panel, no notices badge.
fn list_state(groups: Vec<herdmux::group::RepoGroup>) -> herdmux::state::AppState {
    let mut state = make_state(vec![]);
    state.bottom_panel_height = 0;
    state.notices.missing_hook_groups.clear();
    state.focus_state.focused_pane_id = None;
    state.focus_state.sidebar_focused = false;
    state.repo_groups = groups;
    state.rebuild_row_targets();
    state
}

fn fleet() -> Vec<herdmux::group::RepoGroup> {
    let mut blocked = agent("%4", PaneStatus::Waiting, "schema migration", 60);
    blocked.wait_reason = "permission_prompt".into();
    let mut working = unread(agent("%5", PaneStatus::Running, "load test", 30));
    working.started_at = Some(FIXED_NOW - 30);
    vec![
        make_repo_group(
            "billing",
            vec![
                unread(agent("%1", PaneStatus::Idle, "invoice rounding", 2 * 3600)),
                agent("%2", PaneStatus::Idle, "refund docs", 3 * 3600),
            ],
        ),
        make_repo_group(
            "storefront",
            vec![
                blocked,
                working,
                unread(agent("%3", PaneStatus::Background, "dev server", 600)),
            ],
        ),
    ]
}

#[test]
fn unread_replies_show_the_unread_icon_and_count_in_the_inbox() {
    let mut state = list_state(fleet());
    state.global.view = ViewMode::Compact;

    let output = render_to_string(&mut state, 44, 12);
    insta::assert_snapshot!(output, @"
     ≡5  ◆3  ●1  ◎1  ◐1  ○2
                                             — ▾
    storefront ◐
      ◐ schema migration
      ● load test                    working 30s
    ▌ ◆ dev server
    billing ◆
    ▌ ◆ invoice rounding                  2h ago
      ○ refund docs                       3h ago
    ");
}

#[test]
fn inbox_filter_lists_only_agents_that_need_the_user() {
    let mut state = list_state(fleet());
    state.global.view = ViewMode::Compact;
    state.global.status_filter = StatusFilter::Inbox;
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 44, 12);
    insta::assert_snapshot!(output, @"
     ≡5  ◆3  ●1  ◎1  ◐1  ○2
                                             — ▾
    storefront ◐
      ◐ schema migration
    ▌ ◆ dev server
    billing ◆
    ▌ ◆ invoice rounding                  2h ago
    ");
}

#[test]
fn full_rows_show_the_unread_icon_too() {
    let mut state = list_state(vec![make_repo_group(
        "billing",
        vec![unread(agent(
            "%1",
            PaneStatus::Idle,
            "invoice rounding",
            2 * 3600,
        ))],
    )]);

    let output = render_to_string(&mut state, 44, 8);
    insta::assert_snapshot!(output, @"
     ≡1  ◆1  ○1
                                             — ▾
    billing
    ▌ ◆ claude                            2h ago
    ▌   invoice rounding
    ▌   Waiting for prompt…
    ");
}

#[test]
fn an_unread_reply_is_never_dimmed_as_stale() {
    let mut state = list_state(vec![make_repo_group(
        "billing",
        vec![
            unread(agent("%1", PaneStatus::Idle, "old unread", 26 * 3600)),
            agent("%2", PaneStatus::Idle, "old read", 26 * 3600),
        ],
    )]);
    state.global.view = ViewMode::Compact;

    let output = render_to_styled_string(&mut state, 32, 5);
    insta::assert_snapshot!(output, @"
     ≡[fg:111]2[fg:255]  ◆[fg:245]1[fg:255]  ○[fg:245]2[fg:255]
                                 —[fg:252] ▾[fg:252]
    b[fg:255]i[fg:255]l[fg:255]l[fg:255]i[fg:255]n[fg:255]g[fg:255] [fg:221]◆[fg:221]
    ▌[fg:221] ◆[fg:221] [fg:255,bold]o[fg:255,bold]l[fg:255,bold]d[fg:255,bold] [fg:255,bold]u[fg:255,bold]n[fg:255,bold]r[fg:255,bold]e[fg:255,bold]a[fg:255,bold]d[fg:255,bold]            [fg:252]1[fg:252]d[fg:252] [fg:252]a[fg:252]g[fg:252]o[fg:252]
      ○[fg:110] [fg:243]o[fg:243]l[fg:243]d[fg:243] [fg:243]r[fg:243]e[fg:243]a[fg:243]d[fg:243]              [fg:243]1[fg:243]d[fg:243] [fg:243]a[fg:243]g[fg:243]o[fg:243]
    ");
}

#[test]
fn a_narrow_filter_row_drops_empty_entries_instead_of_clipping() {
    let mut state = list_state(fleet());
    state.global.view = ViewMode::Compact;

    let output = render_to_string(&mut state, 21, 3);
    insta::assert_snapshot!(output, @"
     ≡5 ◆3 ●1 ◎1 ◐1 ○2
                      — ▾
    storefront ◐
    ");
}
