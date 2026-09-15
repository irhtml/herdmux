use crate::tmux;

pub const SPAWNED_OPTION: &str = "@agent-sidebar-spawned";
pub const SPAWNED_FROM_OPTION: &str = "@agent-sidebar-spawned-from";
pub const SPAWNED_WORKTREE_OPTION: &str = "@agent-sidebar-spawned-worktree";
pub const SPAWNED_BRANCH_OPTION: &str = "@agent-sidebar-spawned-branch";
pub(super) const SPAWNED_SCOPE_OPTION: &str = "@agent-sidebar-spawned-scope";

/// Build the tmux `display-message` template used by [`read_spawn_markers`]. One
/// call, six fields: the truthy flag, owning repo, worktree path,
/// branch name, window id, and ownership scope. Callers share this
/// template so the remove confirmation popup and the remove flow
/// itself always read the same set of fields in the same order.
pub fn spawn_markers_template() -> String {
    [
        format!("#{{{SPAWNED_OPTION}}}"),
        format!("#{{{SPAWNED_FROM_OPTION}}}"),
        format!("#{{{SPAWNED_WORKTREE_OPTION}}}"),
        format!("#{{{SPAWNED_BRANCH_OPTION}}}"),
        "#{window_id}".to_string(),
        format!("#{{{SPAWNED_SCOPE_OPTION}}}"),
    ]
    .join("\n")
}

/// Parsed ownership markers. New splits use pane scope; missing scope
/// identifies dedicated windows created by older versions. Fields default because
/// `display-message` returns empty strings for missing keys —
/// [`SpawnMarkers::is_spawned`] is the canonical check. The remove
/// flow also requires `worktree_path`, `branch`, and `window_id` to
/// be populated and errors out otherwise; `spawn_with` always writes
/// the scope before ownership (with rollback on partial failure),
/// so a pane in the wild either has the full set or the remove flow
/// correctly refuses to touch it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnMarkers {
    pub spawned: bool,
    pub from_repo: String,
    pub worktree_path: String,
    pub branch: String,
    pub window_id: String,
    pub pane_scoped: bool,
}

impl SpawnMarkers {
    pub fn is_spawned(&self) -> bool {
        self.spawned && !self.from_repo.is_empty()
    }

    /// Parse the output of `tmux display-message -p -F spawn_markers_template()`.
    /// Missing / empty fields become `""` / `false` rather than errors.
    pub fn parse(raw: &str) -> Self {
        let mut lines = raw.lines();
        let spawned = lines.next().unwrap_or("") == "1";
        let from_repo = lines.next().unwrap_or("").to_string();
        let worktree_path = lines.next().unwrap_or("").to_string();
        let branch = lines.next().unwrap_or("").to_string();
        let window_id = lines.next().unwrap_or("").to_string();
        let pane_scoped = lines.next().unwrap_or("") == "pane";
        Self {
            spawned,
            from_repo,
            worktree_path,
            branch,
            window_id,
            pane_scoped,
        }
    }
}

/// Read the spawn markers for `pane_id` through tmux `display-message`,
/// which falls through pane → window scope. New markers are pane-local;
/// legacy window markers remain readable for backwards-compatible cleanup.
pub fn read_spawn_markers(pane_id: &str) -> SpawnMarkers {
    SpawnMarkers::parse(&tmux::display_message(pane_id, &spawn_markers_template()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_fields_populated() {
        let raw = "1\n/repo\n/repo/.worktrees/foo\nagent/foo\n@42\n";
        let m = SpawnMarkers::parse(raw);
        assert!(m.spawned);
        assert_eq!(m.from_repo, "/repo");
        assert_eq!(m.worktree_path, "/repo/.worktrees/foo");
        assert_eq!(m.branch, "agent/foo");
        assert_eq!(m.window_id, "@42");
        assert!(m.is_spawned());
        assert!(!m.pane_scoped, "legacy window remains window-owned");
    }

    #[test]
    fn parse_pane_scope() {
        let m = SpawnMarkers::parse("1\n/repo\n/repo/.worktrees/foo\nagent/foo\n@42\npane\n");
        assert!(m.is_spawned());
        assert!(m.pane_scoped);
    }

    #[test]
    fn parse_missing_trailing_fields_default_to_empty() {
        let m = SpawnMarkers::parse("1\n/repo\n");
        assert!(m.spawned);
        assert_eq!(m.from_repo, "/repo");
        assert!(m.worktree_path.is_empty());
        assert!(m.branch.is_empty());
        assert!(m.window_id.is_empty());
    }

    #[test]
    fn parse_empty_input_yields_default() {
        let m = SpawnMarkers::parse("");
        assert_eq!(m, SpawnMarkers::default());
        assert!(!m.is_spawned());
    }

    #[test]
    fn is_spawned_requires_both_flag_and_repo() {
        let mut m = SpawnMarkers {
            spawned: true,
            from_repo: String::new(),
            ..Default::default()
        };
        assert!(!m.is_spawned(), "flag alone is insufficient");
        m.from_repo = "/repo".into();
        assert!(m.is_spawned());
        m.spawned = false;
        assert!(!m.is_spawned(), "repo alone is insufficient");
    }

    #[test]
    fn parse_non_one_flag_is_false() {
        // Only literal "1" counts as spawned; any other value (including
        // "true", "yes", "0") reads as false.
        assert!(!SpawnMarkers::parse("true\n/repo\n").spawned);
        assert!(!SpawnMarkers::parse("0\n/repo\n").spawned);
        assert!(!SpawnMarkers::parse("\n/repo\n").spawned);
    }
}
