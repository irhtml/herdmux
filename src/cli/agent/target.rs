//! Resolve the `<target>` argument of `agent prompt|wait|read` to a pane.

use crate::tmux::PaneLocation;

/// Resolve `query` against the live tmux server.
pub(super) fn lookup(query: &str) -> Result<PaneLocation, String> {
    let panes = crate::tmux::query_pane_locations();
    if panes.is_empty() {
        return Err("no tmux panes found (is the tmux server running?)".into());
    }
    resolve(query, &panes).cloned()
}

/// Accepts a pane id (`%12`), a `session:window.pane` address, an exact
/// `@pane_desc` tag, or an exact worktree name, in that order. Name
/// matches that hit several panes prefer agent panes; if that still
/// leaves more than one, the error lists them instead of guessing.
pub(super) fn resolve<'a>(
    query: &str,
    panes: &'a [PaneLocation],
) -> Result<&'a PaneLocation, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("empty target".into());
    }
    if query.starts_with('%') {
        return panes
            .iter()
            .find(|p| p.pane_id == query)
            .ok_or_else(|| format!("no pane {query}; run `agent list`"));
    }
    if query.contains(':') {
        return panes
            .iter()
            .find(|p| p.address() == query)
            .ok_or_else(|| format!("no pane at {query} (expected session:window.pane)"));
    }
    let by_desc = |p: &&PaneLocation| p.desc == query;
    let by_worktree = |p: &&PaneLocation| p.worktree_name == query;
    for matcher in [&by_desc as &dyn Fn(&&PaneLocation) -> bool, &by_worktree] {
        let mut found: Vec<&PaneLocation> = panes
            .iter()
            .filter(|p| !p.is_sidebar())
            .filter(matcher)
            .collect();
        if found.len() > 1 && found.iter().any(|p| !p.agent.is_empty()) {
            found.retain(|p| !p.agent.is_empty());
        }
        match found.as_slice() {
            [] => continue,
            [only] => return Ok(only),
            many => {
                let list = many
                    .iter()
                    .map(|p| format!("{} ({})", p.pane_id, p.address()))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!("`{query}` matches several panes: {list}"));
            }
        }
    }
    Err(format!("no pane matches `{query}`; run `agent list`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, window: u32, agent: &str, desc: &str, worktree: &str) -> PaneLocation {
        PaneLocation {
            pane_id: id.into(),
            session: "main".into(),
            window_index: window,
            pane_index: 0,
            agent: agent.into(),
            desc: desc.into(),
            worktree_name: worktree.into(),
            ..Default::default()
        }
    }

    fn panes() -> Vec<PaneLocation> {
        vec![
            pane("%1", 1, "claude", "reviewer", ""),
            pane("%2", 2, "codex", "", "fix-login"),
            pane("%3", 3, "", "reviewer", ""),
            pane("%4", 4, "claude", "dup", ""),
            pane("%5", 5, "codex", "dup", ""),
            PaneLocation {
                role: "sidebar".into(),
                ..pane("%6", 1, "", "fix-login-sidebar", "")
            },
        ]
    }

    #[test]
    fn resolves_ids_addresses_tags_and_worktrees() {
        let panes = panes();
        assert_eq!(resolve("%2", &panes).unwrap().pane_id, "%2");
        assert_eq!(resolve("main:3.0", &panes).unwrap().pane_id, "%3");
        assert_eq!(resolve("fix-login", &panes).unwrap().pane_id, "%2");
    }

    #[test]
    fn tag_shared_with_a_shell_pane_prefers_the_agent() {
        assert_eq!(resolve("reviewer", &panes()).unwrap().pane_id, "%1");
    }

    #[test]
    fn ambiguous_tag_lists_candidates() {
        let err = resolve("dup", &panes()).unwrap_err();
        assert!(err.contains("%4") && err.contains("%5"), "{err}");
    }

    #[test]
    fn unknown_targets_fail() {
        let panes = panes();
        assert!(resolve("%99", &panes).is_err());
        assert!(resolve("main:9.9", &panes).is_err());
        assert!(resolve("nobody", &panes).is_err());
        assert!(resolve("fix-login-sidebar", &panes).is_err());
        assert!(resolve(" ", &panes).is_err());
    }
}
