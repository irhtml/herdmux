//! Frames for the compact view, the recent-first order, pins, search,
//! and the transcript-backed goal / title rows.

#[allow(dead_code, unused_imports)]
mod test_helpers;

use std::sync::Arc;

use herdmux::state::{AppMode, Focus, SearchState, SortMode, ViewMode};
use herdmux::tmux::{AgentType, PaneInfo, PaneStatus, PermissionMode};
use herdmux::transcript::SessionDigest;
use test_helpers::*;

fn agent(id: &str, status: PaneStatus) -> PaneInfo {
    let mut p = make_pane(AgentType::Claude, status);
    p.pane_id = id.into();
    p.pane_active = false;
    p
}

fn idle(id: &str, tag: &str, secs_ago: u64) -> PaneInfo {
    let mut p = agent(id, PaneStatus::Idle);
    p.pane_desc = tag.into();
    p.activity_at = Some(FIXED_NOW - secs_ago);
    p
}

fn digest(title: &str, prompts: &[&str]) -> SessionDigest {
    SessionDigest {
        title: title.into(),
        prompts: Arc::new(prompts.iter().map(|p| p.to_string()).collect()),
        last_event_at: None,
    }
}

/// Agent list only: no bottom panel, no notices badge.
fn list_state(groups: Vec<herdmux::group::RepoGroup>) -> herdmux::state::AppState {
    let mut state = make_state(vec![]);
    state.bottom_panel_height = 0;
    state.notices.missing_hook_groups.clear();
    state.focus_state.focused_pane_id = None;
    state.repo_groups = groups;
    state.rebuild_row_targets();
    state
}

fn fleet() -> Vec<herdmux::group::RepoGroup> {
    let mut running = agent("%4", PaneStatus::Running);
    running.pane_desc = "PR 42 client impact".into();
    running.started_at = Some(FIXED_NOW - 23);
    running.prompt = "investigate PR 42".into();
    vec![
        make_repo_group(
            "billing",
            vec![idle("%1", "Dockerfile security fix", 2 * 3600)],
        ),
        make_repo_group(
            "storefront",
            vec![idle("%2", "PHP CVE research", 3 * 3600), running],
        ),
        make_repo_group(
            "checkout-api",
            vec![idle("%3", "PHP 8.3 upgrade", 26 * 3600)],
        ),
    ]
}

#[test]
fn compact_view_shows_one_line_per_agent_and_opens_the_cursor_row() {
    let mut state = list_state(fleet());
    state.global.view = ViewMode::Compact;
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.global.selected_pane_row = 2;

    let output = render_to_string(&mut state, 44, 20);
    insta::assert_snapshot!(output, @"
     ≡4  ●1  ◎0  ◐0  ○3  ✕0
                                             — ▾
    storefront
      ● PR 42 client impact                  23s
      ○ PHP CVE research                      3h
    billing
      ○ claude                                2h
        Dockerfile security fix
        Waiting for prompt…
    checkout-api
      ○ PHP 8.3 upgrade                       1d
    ");
}

#[test]
fn compact_view_opens_the_focused_agent_when_the_sidebar_is_not_focused() {
    let mut state = list_state(fleet());
    state.global.view = ViewMode::Compact;
    state.focus_state.sidebar_focused = false;
    state.focus_state.focused_pane_id = Some("%1".into());

    let output = render_to_string(&mut state, 44, 20);
    insta::assert_snapshot!(output, @"
     ≡4  ●1  ◎0  ◐0  ○3  ✕0
                                             — ▾
    storefront
      ● PR 42 client impact                  23s
      ○ PHP CVE research                      3h
    billing
    ┃ ○ claude                                2h
        Dockerfile security fix
        Waiting for prompt…
    checkout-api
      ○ PHP 8.3 upgrade                       1d
    ");
}

#[test]
fn recent_order_with_a_pinned_agent() {
    let mut groups = fleet();
    groups[2].panes[0].0.pinned = true;
    let mut state = list_state(groups);
    state.global.view = ViewMode::Compact;
    state.global.sort = SortMode::Recent;
    state.focus_state.sidebar_focused = false;
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 44, 20);
    insta::assert_snapshot!(output, @"
     ≡4  ●1  ◎0  ◐0  ○3  ✕0
                                             — ▾
    ★ pinned
      ○ ★ PHP 8.3 upgrade        checkout-api 1d
    recent
      ● PR 42 client impact       storefront 23s
      ○ Dockerfile security fix       billing 2h
      ○ PHP CVE research           storefront 3h
    ");
}

#[test]
fn full_view_pinned_group_names_the_repo() {
    let mut groups = fleet();
    groups[0].panes[0].0.pinned = true;
    let mut state = list_state(groups);
    state.focus_state.sidebar_focused = false;
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 44, 30);
    insta::assert_snapshot!(output, @"
     ≡4  ●1  ◎0  ◐0  ○3  ✕0
                                             — ▾
    ★ pinned
      ○ ★ claude · billing                    2h
        Dockerfile security fix
        Waiting for prompt…
    storefront
      ● claude                               23s
        PR 42 client impact
        investigate PR 42
      ○ claude                                3h
        PHP CVE research
        Waiting for prompt…
    checkout-api
      ○ claude                                1d
        PHP 8.3 upgrade
        Waiting for prompt…
    ");
}

#[test]
fn search_lists_matches_from_prompt_history_with_a_snippet() {
    let mut groups = fleet();
    groups[1].panes[0].0.session_id = Some("sid-cve".into());
    let mut state = list_state(groups);
    state.digests.insert(
        "sid-cve".into(),
        digest(
            "PHP security advisory",
            &[
                "research this advisory",
                "rebuild the dockerfile on php 8.3",
                "check now",
            ],
        ),
    );
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.search = SearchState {
        open: true,
        query: "dockerfile".into(),
        ..SearchState::default()
    };
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 44, 20);
    insta::assert_snapshot!(output, @"
     ≡4  ●1  ◎0  ◐0  ○3  ✕0
    / dockerfile█                              2
      ○ claude · billing                      2h
        Dockerfile security fix
        Waiting for prompt…
      ○ PHP CVE research           storefront 3h
        ↳ rebuild the dockerfile on php 8.3
    ");
}

#[test]
fn search_with_no_match_says_so() {
    let mut state = list_state(fleet());
    state.search = SearchState {
        open: true,
        query: "kubernetes".into(),
        ..SearchState::default()
    };
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 44, 10);
    insta::assert_snapshot!(output, @"
     ≡4  ●1  ◎0  ◐0  ○3  ✕0
    / kubernetes█                       no match
    ");
}

#[test]
fn full_row_uses_the_transcript_after_a_resume() {
    // A resumed session: no tag, no prompt from the hooks yet.
    let mut p = agent("%1", PaneStatus::Idle);
    p.session_id = Some("sid".into());
    p.wait_reason = "session_resumed".into();
    p.permission_mode = PermissionMode::Auto;
    let mut state = list_state(vec![make_repo_group("checkout-api", vec![p])]);
    state.digests.insert(
        "sid".into(),
        SessionDigest {
            last_event_at: Some(FIXED_NOW - 5 * 3600),
            ..digest(
                "PHP 8.3 upgrade",
                &[
                    "we need to update this project to use php 8.3",
                    "deploy it to staging",
                ],
            )
        },
    );
    state.focus_state.sidebar_focused = false;

    let output = render_to_string(&mut state, 44, 12);
    insta::assert_snapshot!(output, @"
     ≡1  ●0  ◎0  ◐0  ○1  ✕0
                                             — ▾
    checkout-api
      ○ claude auto                           5h
        PHP 8.3 upgrade
      ◇ we need to update this project to use
        php 8.3
        resumed
        deploy it to staging
    ");
}

#[test]
fn stale_rows_are_dimmed() {
    let mut state = list_state(vec![make_repo_group(
        "checkout-api",
        vec![idle("%1", "fresh", 60), idle("%2", "old", 30 * 3600)],
    )]);
    state.global.view = ViewMode::Compact;
    state.focus_state.sidebar_focused = false;

    let output = render_to_styled_string(&mut state, 30, 5);
    insta::assert_snapshot!(output, @"
     ≡[fg:111]2[fg:255]  ●[fg:245]0[fg:245]  ◎[fg:245]0[fg:245]  ◐[fg:245]0[fg:245]  ○[fg:245]2[fg:255]  ✕[fg:245]0[fg:245]
                               —[fg:252] ▾[fg:252]
    c[fg:255]h[fg:255]e[fg:255]c[fg:255]k[fg:255]o[fg:255]u[fg:255]t[fg:255]-[fg:255]a[fg:255]p[fg:255]i[fg:255]
      ○[fg:110] [fg:255]f[fg:255]r[fg:255]e[fg:255]s[fg:255]h[fg:255]                   [fg:252]1[fg:252]m[fg:252]
      ○[fg:110] [fg:243]o[fg:243]l[fg:243]d[fg:243]                     [fg:243]1[fg:243]d[fg:243]
    ");
}

#[test]
fn picker_shows_the_search_line_and_every_agent() {
    let mut state = list_state(fleet());
    state.mode = AppMode::Picker;
    state.focus_state.sidebar_focused = true;
    state.begin_search();
    state.rebuild_row_targets();

    let output = render_to_string(&mut state, 60, 16);
    insta::assert_snapshot!(output, @"
     ≡4  ●1  ◎0  ◐0  ○3  ✕0
    / █                                                        4
      ● claude · storefront                                  23s
        PR 42 client impact
        investigate PR 42
      ○ Dockerfile security fix                       billing 2h
      ○ PHP CVE research                           storefront 3h
      ○ PHP 8.3 upgrade                          checkout-api 1d
    ");
}
