//! Flat, position-aware view of every tmux pane.
//!
//! [`PaneInfo`](super::PaneInfo) is built for rendering: it drops window
//! and pane indexes, skips non-agent panes, and its `path` /
//! `session_name` fields hold the hook cwd and Claude's display name.
//! The `agent` and `resume` subcommands need the opposite: every pane,
//! its tmux address, and the raw option values, so they get their own
//! single `list-panes -a` query here.

use super::commands::run_tmux;
use super::options::{
    PANE_AGENT, PANE_CWD, PANE_DESC, PANE_PROMPT, PANE_PROMPT_AT, PANE_PROMPT_SOURCE,
    PANE_RESUME_PENDING, PANE_ROLE, PANE_SESSION_ID, PANE_SPAWNED_BY, PANE_STATUS,
    PANE_WAIT_REASON, PANE_WORKTREE_BRANCH, PANE_WORKTREE_NAME,
};
use super::query::split_tmux_fields;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneLocation {
    pub pane_id: String,
    pub session: String,
    pub window_index: u32,
    pub pane_index: u32,
    /// Number of panes in the containing window. Lets resume detect a
    /// restored window whose layout no longer matches the snapshot.
    pub window_panes: u32,
    pub current_path: String,
    pub current_command: String,
    pub pane_pid: u32,
    pub role: String,
    pub agent: String,
    pub session_id: String,
    pub status: String,
    pub wait_reason: String,
    pub desc: String,
    /// Hook-reported cwd (`@pane_cwd`), empty when no hook has fired.
    pub cwd: String,
    pub worktree_name: String,
    pub worktree_branch: String,
    pub prompt: String,
    pub prompt_source: String,
    pub prompt_at: Option<u64>,
    pub resume_pending: String,
    /// Pane id of the pane that ran `agent spawn` for this one.
    pub spawned_by: String,
}

impl PaneLocation {
    /// `session:window.pane`, the human-readable tmux address.
    pub fn address(&self) -> String {
        format!("{}:{}.{}", self.session, self.window_index, self.pane_index)
    }

    pub fn is_sidebar(&self) -> bool {
        self.role == "sidebar"
    }

    /// Hook cwd when known, otherwise tmux's view of the pane cwd.
    pub fn effective_cwd(&self) -> &str {
        if self.cwd.is_empty() {
            &self.current_path
        } else {
            &self.cwd
        }
    }
}

const FIELDS: &[&str] = &[
    "pane_id",
    "session_name",
    "window_index",
    "pane_index",
    "window_panes",
    "pane_current_path",
    "pane_current_command",
    "pane_pid",
    PANE_ROLE,
    PANE_AGENT,
    PANE_SESSION_ID,
    PANE_STATUS,
    PANE_WAIT_REASON,
    PANE_DESC,
    PANE_CWD,
    PANE_WORKTREE_NAME,
    PANE_WORKTREE_BRANCH,
    PANE_PROMPT,
    PANE_PROMPT_SOURCE,
    PANE_PROMPT_AT,
    PANE_RESUME_PENDING,
    PANE_SPAWNED_BY,
];

fn location_format() -> String {
    FIELDS
        .iter()
        .map(|f| format!("#{{q:{f}}}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Every pane on the server, in tmux order, deduplicated by pane id
/// (grouped sessions list the same pane once per session).
pub fn query_pane_locations() -> Vec<PaneLocation> {
    run_tmux(&["list-panes", "-a", "-F", &location_format()])
        .map(|out| parse_pane_locations(&out))
        .unwrap_or_default()
}

pub(crate) fn parse_pane_locations(output: &str) -> Vec<PaneLocation> {
    let mut seen = std::collections::HashSet::new();
    output
        .lines()
        .filter_map(parse_location_line)
        .filter(|loc| seen.insert(loc.pane_id.clone()))
        .collect()
}

fn parse_location_line(line: &str) -> Option<PaneLocation> {
    let f = split_tmux_fields(line, '|');
    if f.len() != FIELDS.len() || !f[0].starts_with('%') {
        return None;
    }
    let mut it = f.into_iter();
    let mut next = || it.next().unwrap_or_default();
    Some(PaneLocation {
        pane_id: next(),
        session: next(),
        window_index: next().parse().ok()?,
        pane_index: next().parse().ok()?,
        window_panes: next().parse().unwrap_or(0),
        current_path: next(),
        current_command: next(),
        pane_pid: next().parse().unwrap_or(0),
        role: next(),
        agent: next(),
        session_id: next(),
        status: next(),
        wait_reason: next(),
        desc: next(),
        cwd: next(),
        worktree_name: next(),
        worktree_branch: next(),
        prompt: next(),
        prompt_source: next(),
        prompt_at: next().parse().ok(),
        resume_pending: next(),
        spawned_by: next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: &[&str]) -> String {
        assert_eq!(fields.len(), FIELDS.len());
        fields.join("|")
    }

    fn agent_fields() -> Vec<&'static str> {
        vec![
            "%3",
            "main",
            "2",
            "1",
            "3",
            "/home/u/repo",
            "claude",
            "4242",
            "",
            "claude",
            "sid-1",
            "idle",
            "",
            "reviewer",
            "/home/u/repo/sub",
            "wt",
            "feat/x",
            "fix the bug",
            "response",
            "1790000000123",
            "",
            "%1",
        ]
    }

    #[test]
    fn parses_every_field() {
        let locs = parse_pane_locations(&line(&agent_fields()));
        assert_eq!(locs.len(), 1);
        let loc = &locs[0];
        assert_eq!(loc.pane_id, "%3");
        assert_eq!(loc.address(), "main:2.1");
        assert_eq!(loc.window_panes, 3);
        assert_eq!(loc.pane_pid, 4242);
        assert_eq!(loc.agent, "claude");
        assert_eq!(loc.session_id, "sid-1");
        assert_eq!(loc.desc, "reviewer");
        assert_eq!(loc.effective_cwd(), "/home/u/repo/sub");
        assert_eq!(loc.worktree_branch, "feat/x");
        assert_eq!(loc.prompt, "fix the bug");
        assert_eq!(loc.prompt_at, Some(1790000000123));
        assert_eq!(loc.spawned_by, "%1");
        assert!(!loc.is_sidebar());
    }

    #[test]
    fn unescapes_quoted_pipes_and_spaces() {
        let mut fields = agent_fields();
        fields[5] = r"/home/u/my\ repo";
        fields[13] = r"a\|b";
        let loc = &parse_pane_locations(&line(&fields))[0];
        assert_eq!(loc.current_path, "/home/u/my repo");
        assert_eq!(loc.desc, "a|b");
    }

    #[test]
    fn empty_hook_cwd_falls_back_to_current_path() {
        let mut fields = agent_fields();
        fields[14] = "";
        let loc = &parse_pane_locations(&line(&fields))[0];
        assert_eq!(loc.effective_cwd(), "/home/u/repo");
    }

    #[test]
    fn skips_malformed_lines_and_dedupes_grouped_sessions() {
        let mut grouped = agent_fields();
        grouped[1] = "main-2";
        let output = format!(
            "garbage\n{}\n{}\n%9|too|few\n",
            line(&agent_fields()),
            line(&grouped)
        );
        let locs = parse_pane_locations(&output);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].session, "main");
    }

    #[test]
    fn missing_prompt_at_is_none() {
        let mut fields = agent_fields();
        fields[19] = "";
        assert_eq!(parse_pane_locations(&line(&fields))[0].prompt_at, None);
    }
}
