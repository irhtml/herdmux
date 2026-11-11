use super::AppState;
use crate::tmux::{PaneInfo, PaneStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    All,
    /// Agents that need the user: waiting on them, or with an unread reply.
    Inbox,
    Running,
    Background,
    Waiting,
    Idle,
    Error,
}

impl StatusFilter {
    pub const VARIANTS: [StatusFilter; 7] = [
        StatusFilter::All,
        StatusFilter::Inbox,
        StatusFilter::Running,
        StatusFilter::Background,
        StatusFilter::Waiting,
        StatusFilter::Idle,
        StatusFilter::Error,
    ];

    pub fn next(self) -> Self {
        let idx = StatusFilter::VARIANTS
            .iter()
            .position(|v| *v == self)
            .unwrap_or(0);
        StatusFilter::VARIANTS[(idx + 1) % StatusFilter::VARIANTS.len()]
    }

    pub fn prev(self) -> Self {
        let idx = StatusFilter::VARIANTS
            .iter()
            .position(|v| *v == self)
            .unwrap_or(0);
        StatusFilter::VARIANTS
            [(idx + StatusFilter::VARIANTS.len() - 1) % StatusFilter::VARIANTS.len()]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Inbox => "inbox",
            Self::Running => "running",
            Self::Background => "background",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Error => "error",
        }
    }

    /// Parse a tmux-option label into a `StatusFilter`. Unknown values
    /// fall back to `All`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "inbox" => Self::Inbox,
            "running" => Self::Running,
            "background" => Self::Background,
            "waiting" => Self::Waiting,
            "idle" => Self::Idle,
            "error" => Self::Error,
            _ => Self::All,
        }
    }

    pub fn matches(self, pane: &PaneInfo) -> bool {
        match self.status() {
            Some(status) => pane.status == status,
            None if self == StatusFilter::Inbox => pane.needs_user(),
            None => true,
        }
    }

    /// The single status this filter selects. `None` for All and Inbox.
    pub fn status(self) -> Option<PaneStatus> {
        match self {
            StatusFilter::All | StatusFilter::Inbox => None,
            StatusFilter::Running => Some(PaneStatus::Running),
            StatusFilter::Background => Some(PaneStatus::Background),
            StatusFilter::Waiting => Some(PaneStatus::Waiting),
            StatusFilter::Idle => Some(PaneStatus::Idle),
            StatusFilter::Error => Some(PaneStatus::Error),
        }
    }
}

/// Agent counts per filter, as shown in the filter row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub all: usize,
    pub inbox: usize,
    pub running: usize,
    pub background: usize,
    pub waiting: usize,
    pub idle: usize,
    pub error: usize,
}

impl StatusCounts {
    pub fn get(&self, filter: StatusFilter) -> usize {
        match filter {
            StatusFilter::All => self.all,
            StatusFilter::Inbox => self.inbox,
            StatusFilter::Running => self.running,
            StatusFilter::Background => self.background,
            StatusFilter::Waiting => self.waiting,
            StatusFilter::Idle => self.idle,
            StatusFilter::Error => self.error,
        }
    }
}

/// One entry of the filter row and the columns it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterItem {
    pub filter: StatusFilter,
    pub count: usize,
    /// First column of the icon.
    pub x: usize,
    /// Icon plus count, in columns.
    pub width: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepoFilter {
    All,
    Repo(String),
}

impl RepoFilter {
    pub fn as_str(&self) -> &str {
        match self {
            Self::All => "all",
            Self::Repo(name) => name.as_str(),
        }
    }

    /// Parse a tmux-option label into a `RepoFilter`. `""` and `"all"`
    /// map to `All`; any other value is stored as `Repo(name)`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "all" | "" => Self::All,
            name => Self::Repo(name.to_string()),
        }
    }

    pub fn matches_group(&self, group_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Repo(name) => name == group_name,
        }
    }
}

impl AppState {
    /// Count agents per filter across the repo groups the repo filter shows.
    pub fn status_counts(&self) -> StatusCounts {
        let mut counts = StatusCounts::default();
        for group in &self.repo_groups {
            if !self.global.repo_filter.matches_group(&group.name) {
                continue;
            }
            for (pane, _) in &group.panes {
                match pane.status {
                    PaneStatus::Running => counts.running += 1,
                    PaneStatus::Background => counts.background += 1,
                    PaneStatus::Waiting => counts.waiting += 1,
                    PaneStatus::Idle => counts.idle += 1,
                    PaneStatus::Error => counts.error += 1,
                    PaneStatus::Unknown => continue,
                }
                counts.all += 1;
                if pane.needs_user() {
                    counts.inbox += 1;
                }
            }
        }
        counts
    }

    /// Icon of a filter row entry.
    pub fn filter_icon(&self, filter: StatusFilter) -> &str {
        match filter {
            StatusFilter::All => self.icons.all_icon(),
            StatusFilter::Inbox => self.icons.unread_icon(),
            _ => filter
                .status()
                .map_or("", |status| self.icons.status_icon(&status)),
        }
    }

    /// Lay out the filter row for a sidebar `width` columns wide (`0`: no
    /// limit). Shared by the renderer and the click handler. Empty entries
    /// other than All and the selected one are left out, so every count on
    /// the row says something. When the row does not fit with two-space
    /// gaps, it uses single spaces.
    pub fn filter_items(&self, width: u16) -> Vec<FilterItem> {
        let counts = self.status_counts();
        let mut entries: Vec<(StatusFilter, usize, usize)> = StatusFilter::VARIANTS
            .iter()
            .map(|&filter| {
                let count = counts.get(filter);
                let cols = crate::ui::text::display_width(self.filter_icon(filter))
                    + count.to_string().len();
                (filter, count, cols)
            })
            .collect();
        let row_width = |entries: &[(StatusFilter, usize, usize)], gap: usize| {
            1 + entries.iter().map(|e| e.2).sum::<usize>() + gap * entries.len().saturating_sub(1)
        };
        let selected = self.global.status_filter;
        entries.retain(|&(filter, count, _)| {
            count > 0 || filter == StatusFilter::All || filter == selected
        });
        let mut gap = 2;
        if width > 0 && row_width(&entries, gap) > width as usize {
            gap = 1;
        }
        let mut x = 1;
        entries
            .into_iter()
            .map(|(filter, count, width)| {
                let item = FilterItem {
                    filter,
                    count,
                    x,
                    width,
                };
                x += width + gap;
                item
            })
            .collect()
    }

    /// Return list of repo names for the popup: ["All", repo1, repo2, ...]
    pub fn repo_names(&self) -> Vec<String> {
        let mut names = vec!["All".to_string()];
        for group in &self.repo_groups {
            names.push(group.name.clone());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::PaneStatus;

    // ─── StatusFilter tests ───────────────────────────────────────────

    #[test]
    fn status_filter_next_cycles() {
        assert_eq!(StatusFilter::All.next(), StatusFilter::Inbox);
        assert_eq!(StatusFilter::Inbox.next(), StatusFilter::Running);
        assert_eq!(StatusFilter::Running.next(), StatusFilter::Background);
        assert_eq!(StatusFilter::Background.next(), StatusFilter::Waiting);
        assert_eq!(StatusFilter::Waiting.next(), StatusFilter::Idle);
        assert_eq!(StatusFilter::Idle.next(), StatusFilter::Error);
        assert_eq!(StatusFilter::Error.next(), StatusFilter::All);
    }

    #[test]
    fn status_filter_prev_cycles() {
        assert_eq!(StatusFilter::All.prev(), StatusFilter::Error);
        assert_eq!(StatusFilter::Error.prev(), StatusFilter::Idle);
        assert_eq!(StatusFilter::Idle.prev(), StatusFilter::Waiting);
        assert_eq!(StatusFilter::Waiting.prev(), StatusFilter::Background);
        assert_eq!(StatusFilter::Background.prev(), StatusFilter::Running);
        assert_eq!(StatusFilter::Running.prev(), StatusFilter::Inbox);
        assert_eq!(StatusFilter::Inbox.prev(), StatusFilter::All);
    }

    #[test]
    fn status_filter_matches_status() {
        use PaneStatus::*;
        let statuses = [Running, Background, Waiting, Idle, Error];
        for status in statuses.clone() {
            let pane = test_pane("%1", status.clone());
            assert!(StatusFilter::All.matches(&pane));
            for filter in StatusFilter::VARIANTS.into_iter().skip(2) {
                assert_eq!(
                    filter.matches(&pane),
                    filter.status() == Some(status.clone()),
                    "{filter:?} on {status:?}"
                );
            }
        }
    }

    #[test]
    fn inbox_holds_waiting_agents_and_unread_replies_but_not_new_turns() {
        use PaneStatus::*;
        let unread = |status: PaneStatus| PaneInfo {
            unread: true,
            ..test_pane("%1", status)
        };
        let inbox = |pane: &PaneInfo| StatusFilter::Inbox.matches(pane);
        assert!(inbox(&test_pane("%1", Waiting)), "blocked on the user");
        assert!(inbox(&unread(Idle)));
        assert!(inbox(&unread(Background)));
        assert!(inbox(&unread(Error)));
        assert!(!inbox(&unread(Running)), "a new turn already started");
        assert!(!inbox(&test_pane("%1", Idle)), "read already");
        assert!(!inbox(&test_pane("%1", Error)), "an old error the user saw");
    }

    #[test]
    fn inbox_label_round_trips() {
        assert_eq!(StatusFilter::from_label("inbox"), StatusFilter::Inbox);
        assert_eq!(StatusFilter::Inbox.as_str(), "inbox");
    }

    // ─── StatusFilter as_str / from_str tests ─────────────────────────

    #[test]
    fn status_filter_as_str_all_variants() {
        assert_eq!(StatusFilter::All.as_str(), "all");
        assert_eq!(StatusFilter::Running.as_str(), "running");
        assert_eq!(StatusFilter::Background.as_str(), "background");
        assert_eq!(StatusFilter::Waiting.as_str(), "waiting");
        assert_eq!(StatusFilter::Idle.as_str(), "idle");
        assert_eq!(StatusFilter::Error.as_str(), "error");
    }

    #[test]
    fn status_filter_from_str_all_variants() {
        assert_eq!(StatusFilter::from_label("all"), StatusFilter::All);
        assert_eq!(StatusFilter::from_label("running"), StatusFilter::Running);
        assert_eq!(
            StatusFilter::from_label("background"),
            StatusFilter::Background
        );
        assert_eq!(StatusFilter::from_label("waiting"), StatusFilter::Waiting);
        assert_eq!(StatusFilter::from_label("idle"), StatusFilter::Idle);
        assert_eq!(StatusFilter::from_label("error"), StatusFilter::Error);
    }

    #[test]
    fn status_filter_from_str_unknown_defaults_to_all() {
        assert_eq!(StatusFilter::from_label(""), StatusFilter::All);
        assert_eq!(StatusFilter::from_label("unknown"), StatusFilter::All);
        assert_eq!(StatusFilter::from_label("Running"), StatusFilter::All); // case-sensitive
    }

    #[test]
    fn status_filter_roundtrip() {
        for filter in StatusFilter::VARIANTS {
            assert_eq!(StatusFilter::from_label(filter.as_str()), filter);
        }
    }

    // ─── RepoFilter tests ─────────────────────────────────────

    #[test]
    fn repo_filter_persistence_roundtrip() {
        assert_eq!(RepoFilter::from_label("all"), RepoFilter::All);
        assert_eq!(RepoFilter::from_label(""), RepoFilter::All);
        assert_eq!(
            RepoFilter::from_label("my-app"),
            RepoFilter::Repo("my-app".into())
        );
        assert_eq!(RepoFilter::All.as_str(), "all");
        assert_eq!(RepoFilter::Repo("my-app".into()).as_str(), "my-app");
    }

    #[test]
    fn repo_filter_matches_group() {
        assert!(RepoFilter::All.matches_group("anything"));
        assert!(RepoFilter::Repo("app".into()).matches_group("app"));
        assert!(!RepoFilter::Repo("app".into()).matches_group("other"));
    }

    // ─── AppState status_counts / repo_names ─────────────────────────

    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::tmux::{AgentType, PermissionMode, WorktreeMetadata};

    fn test_pane(id: &str, status: PaneStatus) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status,
            attention: false,
            agent: AgentType::Claude,
            path: "/tmp".into(),
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
            unread: false,
            window_name: String::new(),
            tmux_session: String::new(),
            bg_shell_cmd: None,
        }
    }

    #[test]
    fn status_counts_on_empty_state_is_all_zeroes() {
        let state = AppState::new("%99".into());
        assert_eq!(state.status_counts(), StatusCounts::default());
    }

    #[test]
    fn status_counts_sums_across_repo_groups_and_filters() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "app".into(),
                has_focus: true,
                panes: vec![
                    (test_pane("%1", PaneStatus::Running), PaneGitInfo::default()),
                    (test_pane("%2", PaneStatus::Idle), PaneGitInfo::default()),
                    (
                        test_pane("%4", PaneStatus::Background),
                        PaneGitInfo::default(),
                    ),
                ],
            },
            RepoGroup {
                name: "lib".into(),
                has_focus: false,
                panes: vec![(test_pane("%3", PaneStatus::Waiting), PaneGitInfo::default())],
            },
        ];

        // All repos: 4 total, the waiting one is in the inbox
        assert_eq!(
            state.status_counts(),
            StatusCounts {
                all: 4,
                inbox: 1,
                running: 1,
                background: 1,
                waiting: 1,
                idle: 1,
                error: 0,
            }
        );

        // Restrict to "app"
        state.global.repo_filter = RepoFilter::Repo("app".into());
        assert_eq!(
            state.status_counts(),
            StatusCounts {
                all: 3,
                inbox: 0,
                running: 1,
                background: 1,
                waiting: 0,
                idle: 1,
                error: 0,
            }
        );
    }

    #[test]
    fn status_counts_counts_unread_replies_in_the_inbox() {
        let mut state = AppState::new("%99".into());
        let unread_idle = PaneInfo {
            unread: true,
            ..test_pane("%1", PaneStatus::Idle)
        };
        state.repo_groups = vec![RepoGroup {
            name: "app".into(),
            has_focus: true,
            panes: vec![
                (unread_idle, PaneGitInfo::default()),
                (test_pane("%2", PaneStatus::Idle), PaneGitInfo::default()),
            ],
        }];
        let counts = state.status_counts();
        assert_eq!((counts.all, counts.idle, counts.inbox), (2, 2, 1));
    }

    fn layout(state: &AppState, width: u16) -> Vec<(StatusFilter, usize, usize)> {
        state
            .filter_items(width)
            .into_iter()
            .map(|item| (item.filter, item.x, item.width))
            .collect()
    }

    #[test]
    fn filter_items_use_two_space_gaps_when_they_fit() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![RepoGroup {
            name: "app".into(),
            has_focus: true,
            panes: [
                PaneStatus::Running,
                PaneStatus::Background,
                PaneStatus::Waiting,
                PaneStatus::Idle,
                PaneStatus::Error,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, status)| (test_pane(&format!("%{i}"), status), PaneGitInfo::default()))
            .collect(),
        }];
        // " ≡5  ◆1  ●1  ◎1  ◐1  ○1  ✕1" is 27 columns.
        assert_eq!(
            layout(&state, 27),
            vec![
                (StatusFilter::All, 1, 2),
                (StatusFilter::Inbox, 5, 2),
                (StatusFilter::Running, 9, 2),
                (StatusFilter::Background, 13, 2),
                (StatusFilter::Waiting, 17, 2),
                (StatusFilter::Idle, 21, 2),
                (StatusFilter::Error, 25, 2),
            ]
        );
        assert_eq!(layout(&state, 0), layout(&state, 27), "0 means no limit");
    }

    #[test]
    fn filter_items_leave_out_empty_entries_but_keep_all_and_the_selection() {
        let mut state = AppState::new("%99".into());
        assert_eq!(layout(&state, 0), vec![(StatusFilter::All, 1, 2)]);
        state.global.status_filter = StatusFilter::Waiting;
        assert_eq!(
            layout(&state, 0),
            vec![(StatusFilter::All, 1, 2), (StatusFilter::Waiting, 5, 2)]
        );
    }

    #[test]
    fn narrow_filter_items_use_single_space_gaps() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![RepoGroup {
            name: "app".into(),
            has_focus: true,
            panes: vec![(test_pane("%1", PaneStatus::Idle), PaneGitInfo::default())],
        }];
        state.global.status_filter = StatusFilter::Error;
        assert_eq!(
            layout(&state, 10),
            vec![
                (StatusFilter::All, 1, 2),
                (StatusFilter::Idle, 4, 2),
                (StatusFilter::Error, 7, 2),
            ]
        );
    }

    #[test]
    fn repo_names_leads_with_all_sentinel() {
        let mut state = AppState::new("%99".into());
        assert_eq!(state.repo_names(), vec!["All"]);
        state.repo_groups = vec![
            RepoGroup {
                name: "alpha".into(),
                has_focus: true,
                panes: vec![],
            },
            RepoGroup {
                name: "beta".into(),
                has_focus: false,
                panes: vec![],
            },
        ];
        assert_eq!(state.repo_names(), vec!["All", "alpha", "beta"]);
    }
}
