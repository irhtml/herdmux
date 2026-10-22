use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::SPAWN_BUTTON;
use super::row::{self, RowExtras};
use crate::group::RepoGroup;
use crate::state::{AppState, Focus, HeaderKind, ListItem, PaneItem, ViewMode};
use crate::ui::text::display_width;

#[derive(Debug, Default)]
pub(super) struct CollectedRows {
    pub lines: Vec<Line<'static>>,
    pub line_to_row: Vec<Option<usize>>,
    pub pending_spawn: Vec<(usize, String, String)>,
    pub pending_remove: Vec<(usize, u16, String)>,
}

pub(super) fn collect(state: &AppState, width: u16) -> CollectedRows {
    let width = width as usize;
    let theme = &state.theme;

    let mut collected = CollectedRows::default();
    let compact = state.global.view == ViewMode::Compact || state.search.open;
    let has_cursor = state.focus_state.sidebar_focused && state.focus_state.focus == Focus::Panes;
    let mut first_header = true;
    let mut row_index: usize = 0;

    for item in state.list_items() {
        let pane_item = match item {
            ListItem::Header(kind) => {
                if !first_header && !compact {
                    // Separate groups, but do not add a leading blank before
                    // the first one so the list starts right below the header.
                    collected.lines.push(Line::from(""));
                    collected.line_to_row.push(None);
                }
                first_header = false;
                let line = match kind {
                    HeaderKind::Repo(group) => repo_header(state, group, width, &mut collected),
                    HeaderKind::Pinned => Line::from(Span::styled(
                        format!("{PIN_HEADER_GLYPH} pinned"),
                        Style::default().fg(theme.accent),
                    )),
                    HeaderKind::Recent => Line::from(Span::styled(
                        "recent",
                        Style::default().fg(theme.text_muted),
                    )),
                };
                collected.lines.push(line);
                collected.line_to_row.push(None);
                continue;
            }
            ListItem::Pane(pane_item) => pane_item,
        };
        let PaneItem {
            pane,
            git: git_info,
            repo,
            show_repo,
            snippet,
        } = pane_item;
        first_header = false;

        let is_selected = has_cursor && row_index == state.cursor();
        let is_active = state.focus_state.focused_pane_id.as_ref() == Some(&pane.pane_id);
        // Compact rows open up for the cursor row, or for the focused agent
        // while the sidebar does not have the keyboard.
        let expanded = !compact || is_selected || (!has_cursor && is_active);

        let extras = RowExtras {
            digest: state.digest_for(pane),
            last_active: state.last_active(pane),
            stale: state.is_stale(pane),
            repo: show_repo.then_some(repo),
            snippet,
        };
        let pane_state = state.pane_state(&pane.pane_id);
        let ports = pane_state.map(|s| s.ports.as_slice());
        let task_progress = pane_state.and_then(|s| s.task_progress.as_ref());
        let status_line_idx = collected.lines.len();
        let pane_lines = if expanded {
            row::render_pane_lines(
                pane,
                git_info,
                ports,
                task_progress,
                is_selected,
                is_active,
                width,
                &state.icons,
                theme,
                state.spinner_frame,
                state.now,
                &extras,
            )
        } else {
            row::render_compact_lines(
                pane,
                is_active,
                width,
                &state.icons,
                theme,
                state.spinner_frame,
                state.now,
                &extras,
            )
        };
        let pane_line_count = pane_lines.len();
        collected.lines.extend(pane_lines);
        for _ in 0..pane_line_count {
            collected.line_to_row.push(Some(row_index));
        }

        // The branch row is always `status_line_idx + 1` when
        // `branch_ports_row` emits a line (which requires a
        // non-empty branch). Look up the exact column of the
        // trailing `×` from the row helper so the click target
        // lines up with the rendered glyph even when the branch
        // name truncates.
        if expanded
            && pane.sidebar_spawned
            && git_info.is_worktree
            && pane_line_count >= 2
            && let Some(x) =
                row::sidebar_remove_marker_col(git_info, ports, true, width.saturating_sub(2))
        {
            collected
                .pending_remove
                .push((status_line_idx + 1, x, pane.pane_id.clone()));
        }

        row_index += 1;
    }

    collected
}

/// Glyph of the pinned group header.
const PIN_HEADER_GLYPH: &str = "★";

/// Plain repo header at column 0, with a `+` spawn button right-aligned on
/// the same row when the group has a resolved repo root (panes outside a
/// git repo get a plain title).
fn repo_header(
    state: &AppState,
    group: &RepoGroup,
    width: usize,
    collected: &mut CollectedRows,
) -> Line<'static> {
    let theme = &state.theme;
    let group_has_focused_pane = state
        .focus_state
        .focused_pane_id
        .as_ref()
        .is_some_and(|fid| group.panes.iter().any(|(p, _)| p.pane_id == *fid));
    let title = &group.name;
    let title_color = if group_has_focused_pane {
        theme.accent
    } else {
        theme.text_active
    };
    let repo_root = group
        .panes
        .iter()
        .find_map(|(_, git)| git.repo_root.clone());
    let spans: Vec<Span<'static>> = if let Some(ref root) = repo_root {
        let title_w = display_width(title);
        let pad_width = width
            .saturating_sub(title_w)
            .saturating_sub(SPAWN_BUTTON.len());
        collected
            .pending_spawn
            .push((collected.lines.len(), group.name.clone(), root.clone()));
        vec![
            Span::styled(title.clone(), Style::default().fg(title_color)),
            Span::raw(" ".repeat(pad_width)),
            Span::styled(SPAWN_BUTTON, Style::default().fg(title_color)),
        ]
    } else {
        vec![Span::styled(
            title.clone(),
            Style::default().fg(title_color),
        )]
    };
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::state::{AppState, StatusFilter};
    use crate::tmux::{AgentType, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata};

    fn make_pane(id: &str, status: PaneStatus) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status,
            attention: false,
            agent: AgentType::Claude,
            path: "/tmp/repo".into(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            pane_desc: String::new(),
            sidebar_spawned: false,
            spawned_by: None,
            pinned: false,
            activity_at: None,
            window_name: String::new(),
            tmux_session: String::new(),
            bg_shell_cmd: None,
        }
    }

    #[test]
    fn collect_empty_repo_groups_produces_no_lines() {
        let state = AppState::new("%0".into());
        let collected = collect(&state, 40);
        assert!(collected.lines.is_empty());
        assert!(collected.line_to_row.is_empty());
        assert!(collected.pending_spawn.is_empty());
        assert!(collected.pending_remove.is_empty());
    }

    #[test]
    fn collect_skips_group_when_status_filter_excludes_all_panes() {
        let mut state = AppState::new("%0".into());
        // The group has only Running panes, so filter to Waiting to drop them all.
        state.global.status_filter = StatusFilter::Waiting;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), PaneGitInfo::default())],
        }];
        let collected = collect(&state, 40);
        assert!(collected.lines.is_empty());
        assert!(collected.pending_spawn.is_empty());
    }

    #[test]
    fn collect_records_pending_spawn_when_repo_root_present() {
        let mut state = AppState::new("%0".into());
        let git_info = PaneGitInfo {
            repo_root: Some("/tmp/repo".into()),
            branch: None,
            is_worktree: false,
            worktree_name: None,
        };
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), git_info)],
        }];
        let collected = collect(&state, 40);
        assert_eq!(
            collected.pending_spawn.len(),
            1,
            "groups with a repo_root should emit a spawn target"
        );
        assert_eq!(collected.pending_spawn[0].1, "repo");
        assert_eq!(collected.pending_spawn[0].2, "/tmp/repo");
        // At least the header plus one pane row should have been pushed.
        assert!(!collected.lines.is_empty());
    }

    #[test]
    fn collect_no_pending_spawn_without_repo_root() {
        let mut state = AppState::new("%0".into());
        state.repo_groups = vec![RepoGroup {
            name: "raw-path".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), PaneGitInfo::default())],
        }];
        let collected = collect(&state, 40);
        assert!(
            collected.pending_spawn.is_empty(),
            "groups without repo_root must not produce spawn targets"
        );
    }

    #[test]
    fn collect_pending_spawn_grows_with_repo_root_bearing_groups() {
        let mut state = AppState::new("%0".into());
        let with_root = |root: &str, name: &str, pane_id: &str| RepoGroup {
            name: name.into(),
            has_focus: false,
            panes: vec![(
                make_pane(pane_id, PaneStatus::Running),
                PaneGitInfo {
                    repo_root: Some(root.into()),
                    branch: None,
                    is_worktree: false,
                    worktree_name: None,
                },
            )],
        };
        state.repo_groups = vec![
            with_root("/repo/a", "a", "%1"),
            with_root("/repo/b", "b", "%2"),
            with_root("/repo/c", "c", "%3"),
        ];
        let collected = collect(&state, 40);
        assert_eq!(collected.pending_spawn.len(), 3);
    }
}
