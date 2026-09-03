use super::commands::{display_message, run_tmux};

/// Snapshot of the sidebar's own pane, taken by the per-second refresh tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarPaneInfo {
    /// The sidebar pane itself holds tmux focus.
    pub focused: bool,
    /// The sidebar's window is its session's current window.
    pub window_active: bool,
    /// At least one attached client is viewing the sidebar's window, so
    /// what it renders is actually on a screen. Falls back to
    /// `window_active` on tmux < 3.1 (no `window_active_clients`), and to
    /// `true` when the probe could not be parsed at all, so a surprise
    /// never silently degrades a sidebar someone is looking at.
    pub visible: bool,
    pub width: u16,
    pub height: u16,
}

pub fn get_sidebar_pane_info(tmux_pane: &str) -> SidebarPaneInfo {
    let out = display_message(
        tmux_pane,
        "#{pane_active} #{window_active} #{pane_width} #{pane_height} #{window_active_clients}",
    );
    parse_sidebar_pane_info(&out)
}

/// Parse the `display-message` line requested by [`get_sidebar_pane_info`].
/// `window_active_clients` is deliberately the last field: an older tmux
/// expands it to nothing, which leaves the four leading fields intact.
pub fn parse_sidebar_pane_info(out: &str) -> SidebarPaneInfo {
    let parts: Vec<&str> = out.splitn(5, ' ').collect();
    if parts.len() < 4 {
        return SidebarPaneInfo {
            focused: false,
            window_active: false,
            visible: true,
            width: 28,
            height: 24,
        };
    }
    let window_active = parts[1] == "1";
    let visible = parts
        .get(4)
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map_or(window_active, |clients| clients > 0);
    SidebarPaneInfo {
        focused: parts[0] == "1",
        window_active,
        visible,
        width: parts[2].parse().unwrap_or(28),
        height: parts[3].parse().unwrap_or(24),
    }
}

pub fn get_pane_path(pane_id: &str) -> Option<String> {
    Some(display_message(pane_id, "#{pane_current_path}")).filter(|s| !s.is_empty())
}

/// Query tmux for all panes in the active window, returning (pane_id, pane_active, path).
/// This queries tmux directly and is NOT filtered by agent type, so it includes
/// all panes (shell, editor, etc.) — not just agent panes.
pub fn query_active_window_panes() -> Vec<(String, bool, String)> {
    // List panes in the current (active) window across all sessions
    let output = match run_tmux(&[
        "list-panes",
        "-F",
        "#{pane_id}|#{pane_active}|#{pane_current_path}",
    ]) {
        Some(s) => s,
        None => return vec![],
    };
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 {
                return None;
            }
            Some((parts[0].to_string(), parts[1] == "1", parts[2].to_string()))
        })
        .collect()
}

/// Find the focused (non-sidebar) pane ID and path by querying tmux directly.
/// Returns all panes regardless of agent type, so activity/git info can be shown
/// even for non-agent panes.
pub fn find_active_pane(sidebar_pane: &str) -> Option<(String, String)> {
    pick_active_pane(sidebar_pane, &query_active_window_panes())
}

/// Pure logic: pick the active non-sidebar pane from a list.
/// Returns the pane with pane_active=true (excluding sidebar) if one exists.
/// Returns None when the sidebar itself is active or no valid pane is found,
/// so callers can preserve the previously focused pane.
pub(crate) fn pick_active_pane(
    sidebar_pane: &str,
    panes: &[(String, bool, String)],
) -> Option<(String, String)> {
    let valid = |p: &&(String, bool, String)| p.0 != sidebar_pane && !p.2.is_empty();
    panes
        .iter()
        .find(|p| p.1 && valid(p))
        .map(|p| (p.0.clone(), p.2.clone()))
}

/// Find the focused pane's working directory by querying tmux directly.
/// Used by the background git thread which doesn't have access to AppState.
/// Queries all panes (not just agent panes) so git info is available
/// even when the focused pane has no agent running.
pub fn focused_pane_path(sidebar_pane: &str) -> Option<String> {
    find_active_pane(sidebar_pane).map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_active_pane_returns_active_non_sidebar() {
        let panes = vec![
            ("%1".into(), false, "/home".into()),
            ("%2".into(), true, "/work".into()),
            ("%3".into(), false, "/tmp".into()),
        ];
        assert_eq!(
            pick_active_pane("%99", &panes),
            Some(("%2".into(), "/work".into()))
        );
    }

    #[test]
    fn pick_active_pane_skips_sidebar_even_when_marked_active() {
        let panes = vec![("%99".into(), true, "/a".into())];
        assert!(pick_active_pane("%99", &panes).is_none());
    }

    #[test]
    fn pick_active_pane_skips_panes_with_empty_path() {
        let panes = vec![
            ("%1".into(), true, "".into()),
            ("%2".into(), true, "/ok".into()),
        ];
        assert_eq!(
            pick_active_pane("%99", &panes),
            Some(("%2".into(), "/ok".into()))
        );
    }

    #[test]
    fn pick_active_pane_returns_none_for_empty_list() {
        assert!(pick_active_pane("%99", &[]).is_none());
    }

    #[test]
    fn pick_active_pane_returns_none_when_no_active() {
        let panes = vec![
            ("%1".into(), false, "/x".into()),
            ("%2".into(), false, "/y".into()),
        ];
        assert!(pick_active_pane("%99", &panes).is_none());
    }
}

#[cfg(test)]
mod sidebar_pane_info_tests {
    use super::*;

    #[test]
    fn viewed_window_is_visible() {
        let info = parse_sidebar_pane_info("1 1 30 40 1");
        assert!(info.focused);
        assert!(info.window_active);
        assert!(info.visible);
        assert_eq!((info.width, info.height), (30, 40));
    }

    #[test]
    fn active_window_in_detached_session_is_hidden() {
        // `window_active` alone is not enough: nobody is attached.
        let info = parse_sidebar_pane_info("0 1 30 40 0");
        assert!(info.window_active);
        assert!(!info.visible);
    }

    #[test]
    fn background_window_is_hidden() {
        assert!(!parse_sidebar_pane_info("0 0 30 40 0").visible);
    }

    #[test]
    fn old_tmux_without_active_clients_falls_back_to_window_active() {
        // tmux < 3.1 expands `#{window_active_clients}` to "", and
        // `display_message` trims the trailing space away.
        assert!(parse_sidebar_pane_info("0 1 30 40").visible);
        assert!(!parse_sidebar_pane_info("0 0 30 40").visible);
    }

    #[test]
    fn unparseable_probe_is_treated_as_visible() {
        let info = parse_sidebar_pane_info("");
        assert!(info.visible);
        assert!(!info.focused);
        assert_eq!((info.width, info.height), (28, 24));
    }
}
