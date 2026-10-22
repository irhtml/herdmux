use std::cmp::Reverse;
use std::collections::HashMap;

use super::{AppState, SortMode};
use crate::group::{PaneGitInfo, RepoGroup};
use crate::search::Snippet;
use crate::tmux::{self, AgentType, PaneInfo};
use crate::transcript::{DigestUpdate, SessionDigest};

/// Default for `@sidebar_stale_hours`.
pub const DEFAULT_STALE_HOURS: u64 = 4;

/// Seconds after which an idle agent is dimmed, from `@sidebar_stale_hours`
/// (fractions allowed, `0` turns dimming off).
pub fn stale_after_from_options(opts: &HashMap<String, String>) -> u64 {
    opts.get(tmux::SIDEBAR_STALE_HOURS)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|hours| hours.is_finite() && *hours >= 0.0)
        .map_or(DEFAULT_STALE_HOURS * 3600, |hours| (hours * 3600.0) as u64)
}

/// A header line in the agent list.
#[derive(Debug)]
pub enum HeaderKind<'a> {
    /// A repository; its agents follow.
    Repo(&'a RepoGroup),
    /// The pinned agents, listed first.
    Pinned,
    /// The unpinned agents in the recent-first order, shown only when
    /// pinned agents come before them.
    Recent,
}

/// An agent row in the agent list.
#[derive(Debug)]
pub struct PaneItem<'a> {
    pub pane: &'a PaneInfo,
    pub git: &'a PaneGitInfo,
    /// Repository name of the row, shown on it when `show_repo` is set.
    pub repo: &'a str,
    /// The row is not under its repository header, so it names the repo.
    pub show_repo: bool,
    /// Matched prompt to show under the row while searching.
    pub snippet: Option<&'a Snippet>,
}

impl<'a> PaneItem<'a> {
    fn new(
        group: &'a RepoGroup,
        pane: &'a PaneInfo,
        git: &'a PaneGitInfo,
        show_repo: bool,
    ) -> Self {
        Self {
            pane,
            git,
            repo: &group.name,
            show_repo,
            snippet: None,
        }
    }
}

/// One entry of the agent list, in display order.
#[derive(Debug)]
pub enum ListItem<'a> {
    Header(HeaderKind<'a>),
    Pane(PaneItem<'a>),
}

impl AppState {
    /// Transcript digest of a Claude pane, once the worker has read it.
    pub fn digest_for(&self, pane: &PaneInfo) -> Option<&SessionDigest> {
        if pane.agent != AgentType::Claude {
            return None;
        }
        self.digests.get(pane.session_id.as_deref()?)
    }

    /// When the agent last did something, in epoch seconds. A working or
    /// waiting agent counts as active now.
    pub fn last_active(&self, pane: &PaneInfo) -> Option<u64> {
        if pane.status.is_active() {
            return Some(self.now);
        }
        let from_transcript = self.digest_for(pane).and_then(|d| d.last_event_at);
        pane.activity_at.max(from_transcript).or(pane.started_at)
    }

    /// An idle agent that has not done anything for `stale_after_secs`.
    /// Pinned agents never go stale.
    pub fn is_stale(&self, pane: &PaneInfo) -> bool {
        if self.stale_after_secs == 0 || pane.pinned || pane.status.is_active() {
            return false;
        }
        self.last_active(pane)
            .is_some_and(|at| self.now.saturating_sub(at) >= self.stale_after_secs)
    }

    /// Cursor row within `layout.pane_row_targets`: the search cursor while
    /// searching, otherwise the cursor shared by every sidebar.
    pub fn cursor(&self) -> usize {
        if self.search.open {
            self.search.selected
        } else {
            self.global.selected_pane_row
        }
    }

    /// Move the cursor. The shared cursor is saved to tmux after a pause;
    /// the search cursor stays local.
    pub fn set_cursor(&mut self, row: usize) {
        if self.search.open {
            self.search.selected = row;
        } else {
            self.global.selected_pane_row = row;
            self.global.queue_cursor_save();
        }
    }

    /// The agent list in display order: pinned agents first, then the rest
    /// grouped by repository or in one list, most recently active first
    /// either way, or the search matches while searching.
    pub fn list_items(&self) -> Vec<ListItem<'_>> {
        if self.search.open {
            return self.search_items();
        }
        let status = self.global.status_filter;
        let shown = |pane: &PaneInfo| status.matches(&pane.status);
        let groups: Vec<&RepoGroup> = self
            .repo_groups
            .iter()
            .filter(|g| self.global.repo_filter.matches_group(&g.name))
            .collect();
        let recent_first = |rows: &mut Vec<PaneItem<'_>>| {
            rows.sort_by_key(|row| Reverse(self.last_active(row.pane)));
        };

        let mut items = Vec::new();
        let mut pinned: Vec<PaneItem<'_>> = groups
            .iter()
            .flat_map(|g| g.panes.iter().map(move |(p, git)| (*g, p, git)))
            .filter(|(_, p, _)| p.pinned && shown(p))
            .map(|(g, p, git)| PaneItem::new(g, p, git, true))
            .collect();
        recent_first(&mut pinned);
        let has_pinned = !pinned.is_empty();
        if has_pinned {
            items.push(ListItem::Header(HeaderKind::Pinned));
            items.extend(pinned.into_iter().map(ListItem::Pane));
        }

        match self.global.sort {
            SortMode::Repo => {
                let mut sections: Vec<(&RepoGroup, Vec<PaneItem<'_>>)> = groups
                    .iter()
                    .filter_map(|group| {
                        let mut rows: Vec<PaneItem<'_>> = group
                            .panes
                            .iter()
                            .filter(|(p, _)| !p.pinned && shown(p))
                            .map(|(p, git)| PaneItem::new(group, p, git, false))
                            .collect();
                        recent_first(&mut rows);
                        (!rows.is_empty()).then_some((*group, rows))
                    })
                    .collect();
                // A repository ranks by its most recently active agent, which
                // is its first row. Ties keep the alphabetical order.
                sections.sort_by_key(|(_, rows)| Reverse(self.last_active(rows[0].pane)));
                for (group, rows) in sections {
                    items.push(ListItem::Header(HeaderKind::Repo(group)));
                    items.extend(rows.into_iter().map(ListItem::Pane));
                }
            }
            SortMode::Recent => {
                let mut rows: Vec<PaneItem<'_>> = groups
                    .iter()
                    .flat_map(|g| g.panes.iter().map(move |(p, git)| (*g, p, git)))
                    .filter(|(_, p, _)| !p.pinned && shown(p))
                    .map(|(g, p, git)| PaneItem::new(g, p, git, true))
                    .collect();
                recent_first(&mut rows);
                if has_pinned && !rows.is_empty() {
                    items.push(ListItem::Header(HeaderKind::Recent));
                }
                items.extend(rows.into_iter().map(ListItem::Pane));
            }
        }
        items
    }

    fn search_items(&self) -> Vec<ListItem<'_>> {
        let by_id: HashMap<&str, (&RepoGroup, &PaneInfo, &PaneGitInfo)> = self
            .repo_groups
            .iter()
            .flat_map(|g| {
                g.panes
                    .iter()
                    .map(move |(p, git)| (p.pane_id.as_str(), (g, p, git)))
            })
            .collect();
        self.search
            .hits
            .iter()
            .filter_map(|hit| {
                let (group, pane, git) = by_id.get(hit.pane_id.as_str())?;
                Some(ListItem::Pane(PaneItem {
                    pane,
                    git,
                    repo: &group.name,
                    show_repo: true,
                    snippet: hit.snippet.as_ref(),
                }))
            })
            .collect()
    }

    /// Switch between full and compact rows (the `v` key).
    pub fn toggle_view(&mut self) {
        self.global.view = self.global.view.toggled();
        self.global.save_view();
    }

    /// Switch between repository groups and most-recent-first (the `o` key).
    /// The cursor stays on the same agent.
    pub fn toggle_sort(&mut self) {
        let selected = self.selected_pane().map(|p| p.pane_id.clone());
        self.global.sort = self.global.sort.toggled();
        self.global.save_sort();
        self.rebuild_row_targets();
        self.keep_cursor_on(selected);
    }

    /// Pin or unpin the agent under the cursor (the `p` key). The flag is
    /// a pane option, so every sidebar shows it on its next refresh.
    pub fn toggle_pin_selected(&mut self) {
        let Some(pane) = self.selected_pane() else {
            return;
        };
        let pane_id = pane.pane_id.clone();
        let pinned = !pane.pinned;
        if pinned {
            tmux::set_pane_option(&pane_id, tmux::PANE_PINNED, "1");
        } else {
            tmux::unset_pane_option(&pane_id, tmux::PANE_PINNED);
        }
        for group in &mut self.repo_groups {
            for (p, _) in &mut group.panes {
                if p.pane_id == pane_id {
                    p.pinned = pinned;
                }
            }
        }
        self.rebuild_row_targets();
        self.keep_cursor_on(Some(pane_id));
    }

    /// The row `pane_id` is listed on.
    pub(crate) fn row_of(&self, pane_id: &str) -> Option<usize> {
        self.layout
            .pane_row_targets
            .iter()
            .position(|t| t.pane_id == pane_id)
    }

    /// Move the cursor to `pane_id`'s row, if it is listed.
    pub(crate) fn keep_cursor_on(&mut self, pane_id: Option<String>) {
        if let Some(row) = pane_id.and_then(|id| self.row_of(&id)) {
            self.set_cursor(row);
        }
    }

    /// Claude sessions to read transcripts for: (session id, working
    /// directory), sorted so the worker is only told when the set changes.
    pub fn transcript_sessions(&self) -> Vec<(String, String)> {
        let mut sessions: Vec<(String, String)> = self
            .repo_groups
            .iter()
            .flat_map(|g| g.panes.iter())
            .filter(|(p, _)| p.agent == AgentType::Claude)
            .filter_map(|(p, _)| Some((p.session_id.clone()?, p.path.clone())))
            .collect();
        sessions.sort();
        sessions.dedup_by(|a, b| a.0 == b.0);
        sessions
    }

    /// Apply digests sent by the transcript worker. Returns `true` when
    /// anything changed.
    pub fn apply_digest_updates(&mut self, updates: Vec<DigestUpdate>) -> bool {
        if updates.is_empty() {
            return false;
        }
        for update in updates {
            match update {
                DigestUpdate::Set(sid, digest) => {
                    self.digests.insert(sid, digest);
                }
                DigestUpdate::Remove(sid) => {
                    self.digests.remove(&sid);
                }
            }
        }
        // Prompts feed the search matches and the recent-first order.
        self.rebuild_row_targets();
        true
    }

    /// Pane ids of the agent rows, in display order.
    pub fn listed_pane_ids(&self) -> Vec<String> {
        self.list_items()
            .into_iter()
            .filter_map(|item| match item {
                ListItem::Pane(row) => Some(row.pane.pane_id.clone()),
                ListItem::Header(_) => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{SearchState, StatusFilter};
    use crate::tmux::{PaneStatus, PermissionMode, WorktreeMetadata};
    use std::sync::Arc;

    const NOW: u64 = 1_800_000_000;

    fn pane(id: &str, status: PaneStatus) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status,
            attention: false,
            agent: AgentType::Claude,
            path: "/repo".into(),
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

    fn idle_since(id: &str, secs_ago: u64) -> PaneInfo {
        let mut p = pane(id, PaneStatus::Idle);
        p.activity_at = Some(NOW - secs_ago);
        p
    }

    fn group(name: &str, panes: Vec<PaneInfo>) -> RepoGroup {
        RepoGroup {
            name: name.into(),
            has_focus: false,
            panes: panes
                .into_iter()
                .map(|p| (p, PaneGitInfo::default()))
                .collect(),
        }
    }

    fn state(groups: Vec<RepoGroup>) -> AppState {
        let mut state = AppState::new("%99".into());
        state.now = NOW;
        state.repo_groups = groups;
        state.rebuild_row_targets();
        state
    }

    /// Headers and pane ids in display order, e.g. `["[pinned]", "%2", "[app]", "%1"]`.
    fn layout(state: &AppState) -> Vec<String> {
        state
            .list_items()
            .iter()
            .map(|item| match item {
                ListItem::Header(HeaderKind::Repo(g)) => format!("[{}]", g.name),
                ListItem::Header(HeaderKind::Pinned) => "[pinned]".into(),
                ListItem::Header(HeaderKind::Recent) => "[recent]".into(),
                ListItem::Pane(row) => row.pane.pane_id.clone(),
            })
            .collect()
    }

    fn digest(title: &str, prompts: &[&str], last_event_at: Option<u64>) -> SessionDigest {
        SessionDigest {
            title: title.into(),
            prompts: Arc::new(prompts.iter().map(|p| p.to_string()).collect()),
            last_event_at,
        }
    }

    #[test]
    fn pinned_agents_lead_the_repo_grouped_list() {
        let mut pinned = idle_since("%3", 60);
        pinned.pinned = true;
        let s = state(vec![
            group("api", vec![idle_since("%1", 10), pinned]),
            group("web", vec![idle_since("%2", 5)]),
        ]);
        assert_eq!(layout(&s), ["[pinned]", "%3", "[web]", "%2", "[api]", "%1"]);
        let ids: Vec<_> = s
            .layout
            .pane_row_targets
            .iter()
            .map(|t| &t.pane_id)
            .collect();
        assert_eq!(
            ids,
            ["%3", "%2", "%1"],
            "cursor rows follow the display order"
        );
    }

    #[test]
    fn repo_groups_and_their_agents_put_the_latest_activity_first() {
        let mut running = pane("%4", PaneStatus::Running);
        running.activity_at = Some(NOW - 9000);
        let s = state(vec![
            group("api", vec![idle_since("%1", 7200), idle_since("%2", 60)]),
            group("docs", vec![pane("%5", PaneStatus::Idle)]),
            group("web", vec![idle_since("%3", 3600), running]),
        ]);
        // A running agent counts as active now. An agent with no activity
        // time sorts last, and its repository with it.
        assert_eq!(
            layout(&s),
            ["[web]", "%4", "%3", "[api]", "%2", "%1", "[docs]", "%5"]
        );
    }

    #[test]
    fn recent_order_puts_the_latest_activity_first_and_names_repos() {
        let mut s = state(vec![
            group(
                "api",
                vec![idle_since("%1", 7200), pane("%2", PaneStatus::Running)],
            ),
            group("web", vec![idle_since("%3", 60)]),
        ]);
        s.global.sort = SortMode::Recent;
        s.rebuild_row_targets();
        assert_eq!(layout(&s), ["%2", "%3", "%1"]);
        assert!(s.list_items().iter().all(|item| matches!(
            item,
            ListItem::Pane(row) if row.show_repo
        )));

        s.repo_groups[0].panes[0].0.pinned = true;
        s.rebuild_row_targets();
        assert_eq!(layout(&s), ["[pinned]", "%1", "[recent]", "%2", "%3"]);
    }

    #[test]
    fn cursor_stays_on_its_agent_when_rows_reorder() {
        let mut s = state(vec![
            group("api", vec![idle_since("%1", 7200)]),
            group("web", vec![idle_since("%2", 60)]),
        ]);
        s.global.sort = SortMode::Recent;
        s.rebuild_row_targets();
        s.global.selected_pane_row = 1; // %1, the older one
        s.repo_groups[0].panes[0].0.activity_at = Some(NOW);
        s.rebuild_row_targets();
        assert_eq!(layout(&s), ["%1", "%2"]);
        assert_eq!(s.selected_pane().map(|p| p.pane_id.as_str()), Some("%1"));
    }

    #[test]
    fn status_filter_applies_to_pinned_rows_too() {
        let mut pinned = idle_since("%1", 60);
        pinned.pinned = true;
        let mut s = state(vec![group(
            "api",
            vec![pinned, pane("%2", PaneStatus::Running)],
        )]);
        s.global.status_filter = StatusFilter::Running;
        s.rebuild_row_targets();
        assert_eq!(layout(&s), ["[api]", "%2"]);
    }

    #[test]
    fn last_active_prefers_hooks_and_transcript_over_prompt_time() {
        let mut s = state(vec![]);
        let mut p = pane("%1", PaneStatus::Idle);
        p.session_id = Some("sid".into());
        p.started_at = Some(NOW - 9000);
        assert_eq!(
            s.last_active(&p),
            Some(NOW - 9000),
            "prompt time as a fallback"
        );

        s.digests
            .insert("sid".into(), digest("", &[], Some(NOW - 600)));
        assert_eq!(s.last_active(&p), Some(NOW - 600));

        p.activity_at = Some(NOW - 60);
        assert_eq!(
            s.last_active(&p),
            Some(NOW - 60),
            "newest of hook and transcript"
        );

        p.status = PaneStatus::Waiting;
        assert_eq!(
            s.last_active(&p),
            Some(NOW),
            "a waiting agent is active now"
        );
    }

    #[test]
    fn stale_needs_idle_unpinned_and_old() {
        let mut s = state(vec![]);
        s.stale_after_secs = 3600;
        assert!(s.is_stale(&idle_since("%1", 7200)));
        assert!(!s.is_stale(&idle_since("%1", 600)));
        let mut pinned = idle_since("%1", 7200);
        pinned.pinned = true;
        assert!(!s.is_stale(&pinned));
        assert!(!s.is_stale(&pane("%1", PaneStatus::Idle)), "unknown age");
        s.stale_after_secs = 0;
        assert!(!s.is_stale(&idle_since("%1", 7200)));
    }

    #[test]
    fn stale_hours_option_parses_fractions_and_off() {
        let opts =
            |v: &str| HashMap::from([(tmux::SIDEBAR_STALE_HOURS.to_string(), v.to_string())]);
        assert_eq!(
            stale_after_from_options(&HashMap::new()),
            DEFAULT_STALE_HOURS * 3600
        );
        assert_eq!(stale_after_from_options(&opts("0.5")), 1800);
        assert_eq!(stale_after_from_options(&opts("0")), 0);
        assert_eq!(
            stale_after_from_options(&opts("-2")),
            DEFAULT_STALE_HOURS * 3600
        );
        assert_eq!(
            stale_after_from_options(&opts("soon")),
            DEFAULT_STALE_HOURS * 3600
        );
    }

    #[test]
    fn toggle_pin_moves_the_agent_and_keeps_the_cursor_on_it() {
        let _guard = tmux::test_mock::install();
        let mut s = state(vec![group(
            "api",
            vec![idle_since("%1", 10), idle_since("%2", 20)],
        )]);
        s.global.selected_pane_row = 1;

        s.toggle_pin_selected();
        assert_eq!(
            tmux::test_mock::get("%2", tmux::PANE_PINNED).as_deref(),
            Some("1")
        );
        assert_eq!(layout(&s), ["[pinned]", "%2", "[api]", "%1"]);
        assert_eq!(s.selected_pane().map(|p| p.pane_id.as_str()), Some("%2"));

        s.toggle_pin_selected();
        assert!(!tmux::test_mock::contains("%2", tmux::PANE_PINNED));
        assert_eq!(layout(&s), ["[api]", "%1", "%2"]);
        assert_eq!(s.selected_pane().map(|p| p.pane_id.as_str()), Some("%2"));
    }

    #[test]
    fn search_ranks_tag_matches_above_history_and_ignores_filters() {
        let mut tagged = idle_since("%1", 7200);
        tagged.pane_desc = "Dockerfile security fix".into();
        let mut history = idle_since("%2", 60);
        history.session_id = Some("sid".into());
        let mut other = idle_since("%3", 30);
        other.pane_desc = "nextjs rewrite".into();
        let mut s = state(vec![group("api", vec![tagged, history, other])]);
        s.digests.insert(
            "sid".into(),
            digest(
                "PHP CVE research",
                &["check the cve", "patch the Dockerfile base"],
                None,
            ),
        );
        s.global.status_filter = StatusFilter::Running;
        s.search = SearchState {
            open: true,
            query: "dockerfile".into(),
            ..SearchState::default()
        };
        s.rebuild_row_targets();

        assert_eq!(layout(&s), ["%1", "%2"]);
        assert!(
            s.search.hits[0].snippet.is_none(),
            "tag match is visible already"
        );
        let snippet = s.search.hits[1].snippet.as_ref().unwrap();
        assert_eq!(snippet.text, "patch the Dockerfile base");
    }

    #[test]
    fn empty_search_lists_everything_most_recent_first() {
        let mut s = state(vec![
            group("api", vec![idle_since("%1", 7200)]),
            group("web", vec![idle_since("%2", 60)]),
        ]);
        s.search.open = true;
        s.rebuild_row_targets();
        assert_eq!(layout(&s), ["%2", "%1"]);
    }

    #[test]
    fn search_cursor_does_not_move_the_shared_cursor() {
        let mut s = state(vec![group(
            "api",
            vec![idle_since("%1", 10), idle_since("%2", 20)],
        )]);
        s.global.selected_pane_row = 1;
        s.search.open = true;
        s.rebuild_row_targets();
        assert_eq!(s.cursor(), 0);
        s.set_cursor(1);
        assert!(s.move_pane_selection(-1));
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.global.selected_pane_row, 1);
    }

    #[test]
    fn search_edits_reset_the_cursor_and_delete_words() {
        let mut s = state(vec![group(
            "api",
            vec![idle_since("%1", 10), idle_since("%2", 20)],
        )]);
        s.search.open = true;
        s.rebuild_row_targets();
        s.set_cursor(1);
        for c in "php docker".chars() {
            s.search_push_char(c);
        }
        assert_eq!(s.search.selected, 0);
        s.search_delete_word();
        assert_eq!(s.search.query, "php ");
        s.search_pop_char();
        assert_eq!(s.search.query, "php");
        s.search_clear_query();
        assert_eq!(s.search.query, "");
    }

    #[test]
    fn transcript_sessions_lists_claude_sessions_once() {
        let mut a = pane("%1", PaneStatus::Idle);
        a.session_id = Some("s1".into());
        let mut b = pane("%2", PaneStatus::Idle);
        b.session_id = Some("s1".into());
        let mut codex = pane("%3", PaneStatus::Idle);
        codex.agent = AgentType::Codex;
        codex.session_id = Some("s2".into());
        let s = state(vec![group(
            "api",
            vec![a, b, codex, pane("%4", PaneStatus::Idle)],
        )]);
        assert_eq!(
            s.transcript_sessions(),
            [("s1".to_string(), "/repo".to_string())]
        );
    }

    #[test]
    fn digest_updates_refresh_search_hits() {
        let mut p = idle_since("%1", 10);
        p.session_id = Some("sid".into());
        let mut s = state(vec![group("api", vec![p])]);
        s.search.open = true;
        s.search.query = "nginx".into();
        s.rebuild_row_targets();
        assert!(s.layout.pane_row_targets.is_empty());

        assert!(s.apply_digest_updates(vec![DigestUpdate::Set(
            "sid".into(),
            digest("", &["fix nginx"], None),
        )]));
        assert_eq!(s.layout.pane_row_targets.len(), 1);

        s.apply_digest_updates(vec![DigestUpdate::Remove("sid".into())]);
        assert!(s.layout.pane_row_targets.is_empty());
        assert!(!s.apply_digest_updates(vec![]));
    }

    // ─── search cursor ──────────────────────────────────────────────
    // `open_search` / `close_search` also refresh from tmux; these tests
    // run the same steps with a plain rebuild in place of that refresh.

    fn selected_id(s: &AppState) -> Option<&str> {
        s.selected_pane().map(|p| p.pane_id.as_str())
    }

    fn fleet() -> AppState {
        state(vec![
            group("a", vec![idle_since("%1", 4000), idle_since("%2", 3000)]),
            group("b", vec![idle_since("%3", 100), idle_since("%4", 50)]),
        ])
    }

    fn open_search_without_refresh(s: &mut AppState) {
        s.begin_search();
        s.rebuild_row_targets();
    }

    #[test]
    fn leaving_search_puts_the_cursor_back_on_its_agent() {
        let mut s = fleet();
        s.global.selected_pane_row = s.row_of("%2").unwrap();
        assert_eq!(selected_id(&s), Some("%2"));

        open_search_without_refresh(&mut s);
        s.set_cursor(3);
        let back = s.end_search();
        s.rebuild_row_targets();
        s.keep_cursor_on(back);
        assert_eq!(selected_id(&s), Some("%2"));
    }

    #[test]
    fn search_cursor_stays_on_its_agent_when_hits_reorder() {
        let mut s = fleet();
        open_search_without_refresh(&mut s);
        s.set_cursor(1);
        let chosen = selected_id(&s).map(str::to_string);

        // Another agent ends a turn and moves to the top of the hits.
        s.repo_groups[0].panes[0].0.activity_at = Some(NOW);
        s.rebuild_row_targets();
        assert_eq!(selected_id(&s).map(str::to_string), chosen);
    }

    #[test]
    fn editing_the_query_moves_the_search_cursor_to_the_best_match() {
        let mut s = fleet();
        open_search_without_refresh(&mut s);
        s.set_cursor(2);
        s.search_push_char('x');
        s.search_pop_char();
        assert_eq!(s.search.selected, 0);
    }

    #[test]
    fn ctrl_w_deletes_words_after_any_whitespace() {
        let mut s = fleet();
        open_search_without_refresh(&mut s);
        s.search.query = "php foo\u{a0}bar".into();
        s.search_delete_word();
        assert_eq!(s.search.query, "php foo\u{a0}");
        s.search_delete_word();
        assert_eq!(s.search.query, "php ");
        s.search_delete_word();
        assert_eq!(s.search.query, "");
    }
}
