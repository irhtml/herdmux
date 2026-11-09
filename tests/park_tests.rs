//! Frames for parking: the `z` confirmation and parked sessions in the
//! finder.

#[allow(dead_code, unused_imports)]
mod test_helpers;

use herdmux::park::Parked;
use herdmux::state::{AppMode, Focus, PopupState};
use herdmux::tmux::{AgentType, PaneInfo, PaneStatus};
use test_helpers::*;

fn idle(id: &str, tag: &str, secs_ago: u64) -> PaneInfo {
    let mut p = make_pane(AgentType::Claude, PaneStatus::Idle);
    p.pane_id = id.into();
    p.pane_active = false;
    p.pane_desc = tag.into();
    p.activity_at = Some(FIXED_NOW - secs_ago);
    p
}

fn list_state(groups: Vec<herdmux::group::RepoGroup>) -> herdmux::state::AppState {
    let mut state = make_state(vec![]);
    state.bottom_panel_height = 0;
    state.notices.missing_hook_groups.clear();
    state.focus_state.focused_pane_id = None;
    state.repo_groups = groups;
    state.rebuild_row_targets();
    state
}

fn parked(sid: &str, desc: &str, repo: &str, hours_ago: u64) -> Parked {
    Parked {
        session_id: sid.into(),
        agent: "claude".into(),
        desc: desc.into(),
        repo: repo.into(),
        parked_at: FIXED_NOW - hours_ago * 3600,
        handoff: "Goal: totals on invoices\nState: export done\nNext: add the rounding test".into(),
        ..Default::default()
    }
}

fn picker(cursor: usize) -> herdmux::state::AppState {
    let mut state = list_state(vec![
        make_repo_group("billing", vec![idle("%1", "invoice export", 600)]),
        make_repo_group("storefront", vec![idle("%2", "search ranking", 3600)]),
    ]);
    state.mode = AppMode::Picker;
    state.focus_state.sidebar_focused = true;
    state.begin_search();
    state.parked = vec![
        parked("p1", "invoice totals", "billing", 5),
        Parked {
            handoff: "Next: retry the declined cards".into(),
            ..parked("p2", "checkout retries", "checkout-api", 30)
        },
    ];
    state.rebuild_row_targets();
    state.search.selected = cursor;
    state
}

#[test]
fn the_picker_lists_parked_sessions_under_the_live_agents() {
    let mut state = picker(0);
    let output = render_to_string(&mut state, 60, 14);
    insta::assert_snapshot!(output, @"
     ≡2  ◆0  ●0  ◎0  ◐0  ○2  ✕0
    / █                                                        4
      ○ claude · billing                                     10m
        invoice export
        Waiting for prompt…
      ○ search ranking                             storefront 1h
    parked
      ◌ invoice totals                                billing 5h
      ◌ checkout retries                         checkout-api 1d
    ");
}

#[test]
fn the_cursor_on_a_parked_session_shows_its_next_step() {
    let mut state = picker(2);
    let output = render_to_string(&mut state, 60, 14);
    insta::assert_snapshot!(output, @"
     ≡2  ◆0  ●0  ◎0  ◐0  ○2  ✕0
    / █                                                        4
      ○ invoice export                               billing 10m
      ○ search ranking                             storefront 1h
    parked
      ◌ invoice totals                                billing 5h
        ↳ add the rounding test
      ◌ checkout retries                         checkout-api 1d
    ");
}

#[test]
fn a_parked_session_matches_on_its_handoff() {
    let mut state = picker(0);
    state.search.query = "rounding".into();
    state.rebuild_row_targets();
    state.search.selected = 0;
    let output = render_to_string(&mut state, 44, 10);
    insta::assert_snapshot!(output, @"
     ≡2  ◆0  ●0  ◎0  ◐0  ○2  ✕0
    / rounding█                                1
    parked
      ◌ invoice totals                billing 5h
        ↳ Next: add the rounding test
    ");
}

#[test]
fn the_park_confirmation_fits_a_narrow_sidebar() {
    let mut state = list_state(vec![make_repo_group(
        "billing",
        vec![idle("%1", "invoice export", 600)],
    )]);
    state.focus_state.sidebar_focused = true;
    state.focus_state.focus = Focus::Panes;
    state.popup = PopupState::ParkConfirm {
        pane_id: "%1".into(),
        name: "invoice export".into(),
        area: None,
    };
    let output = render_to_string(&mut state, 21, 14);
    insta::assert_snapshot!(output, @"
     ≡1 ○1
                      — ▾
    billing
    ╭ park invoice expo…╮
    │[y] with handoff   │
    │[s] no handoff     │
    │[n] cancel         │
    │closes the pane    │
    ╰───────────────────╯
    ");
}

#[test]
fn the_park_confirmation_in_a_wide_sidebar() {
    let mut state = list_state(vec![make_repo_group(
        "billing",
        vec![idle("%1", "invoice export", 600)],
    )]);
    state.popup = PopupState::ParkConfirm {
        pane_id: "%1".into(),
        name: "invoice export".into(),
        area: None,
    };
    let output = render_to_string(&mut state, 44, 14);
    insta::assert_snapshot!(output, @"
     ≡1  ◆0  ●0  ◎0  ◐0  ○1  ✕0
                                             — ▾
    billing
      ○ clau╭ park invoice export ─────╮     10m
        invo│[y] with handoff          │
        Wait│[s] no handoff            │
            │[n] cancel                │
            │closes the pane           │
            ╰──────────────────────────╯
    ");
}
