//! Which panes are on screen, for the unread mark (`@pane_unread`).
//!
//! A pane counts as seen when an attached client shows its window. Every
//! pane in that window counts, since side-by-side agents are all in view.
//! A zoomed window shows only its active pane. Sidebar panes never count.

use super::commands::{run_tmux, run_tmux_batch};
use super::options::PANE_UNREAD;

/// One line per pane of a window. `window_active_clients` needs tmux 3.1;
/// older servers expand it to nothing and [`visible_panes`] falls back to
/// `window_active` plus `session_attached`.
const WINDOW_PANES_FORMAT: &str = "#{pane_id}|#{@pane_role}|#{@pane_unread}|#{pane_active}|#{window_zoomed_flag}|#{window_active}|#{session_attached}|#{window_active_clients}";

/// A pane line parsed from [`WINDOW_PANES_FORMAT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowPane {
    pub pane_id: String,
    /// On screen in an attached client.
    pub visible: bool,
    pub unread: bool,
}

/// Parse `list-panes -F WINDOW_PANES_FORMAT` output. Sidebar panes and
/// malformed lines are left out.
pub fn parse_window_panes(output: &str) -> Vec<WindowPane> {
    output
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('|').collect();
            if f.len() < 7 || f[0].is_empty() || f[1] == "sidebar" {
                return None;
            }
            let window_shown = match f.get(7).and_then(|s| s.trim().parse::<u32>().ok()) {
                Some(clients) => clients > 0,
                None => f[5] == "1" && f[6].trim().parse::<u32>().is_ok_and(|n| n > 0),
            };
            let zoomed = f[4] == "1";
            Some(WindowPane {
                pane_id: f[0].to_string(),
                visible: window_shown && (!zoomed || f[3] == "1"),
                unread: f[2] == "1",
            })
        })
        .collect()
}

/// One line per pane, for [`notify_sidebars`].
const SIDEBAR_PIDS_FORMAT: &str =
    "#{@pane_role}|#{@sidebar_pid}|#{window_active}|#{session_attached}|#{window_active_clients}";

/// Pids of the sidebars an attached client shows, from
/// `list-panes -a -F SIDEBAR_PIDS_FORMAT` output. Hidden sidebars are left
/// out: they re-sync when their window is selected.
pub fn parse_visible_sidebar_pids(output: &str) -> Vec<i32> {
    output
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('|').collect();
            if f.len() < 4 || f[0] != "sidebar" {
                return None;
            }
            let shown = match f.get(4).and_then(|s| s.trim().parse::<u32>().ok()) {
                Some(clients) => clients > 0,
                None => f[2] == "1" && f[3].trim().parse::<u32>().is_ok_and(|n| n > 0),
            };
            let pid = f[1].trim().parse::<i32>().ok()?;
            // Never 0, 1 or a negative pid: those address process groups.
            (shown && pid > 1).then_some(pid)
        })
        .collect()
}

/// Tell every sidebar on screen to refresh now (SIGUSR1) instead of on its
/// next one-second tick. Called after a change the sidebar shows: an agent
/// hook that moved a pane's status, or a cleared unread mark.
pub fn notify_sidebars() {
    // Unit tests must not signal the sidebars of the developer's tmux.
    if cfg!(test) {
        return;
    }
    if let Some(out) = run_tmux(&["list-panes", "-a", "-F", SIDEBAR_PIDS_FORMAT]) {
        for pid in parse_visible_sidebar_pids(&out) {
            // SAFETY: plain syscall; a pid that is gone only returns ESRCH.
            unsafe {
                libc::kill(pid, libc::SIGUSR1);
            }
        }
    }
}

/// Panes of the window that holds `target` (a pane or window id).
fn window_panes(target: &str) -> Vec<WindowPane> {
    run_tmux(&["list-panes", "-t", target, "-F", WINDOW_PANES_FORMAT])
        .map(|out| parse_window_panes(&out))
        .unwrap_or_default()
}

/// Whether an attached client shows `pane` right now.
pub fn pane_on_screen(pane: &str) -> bool {
    #[cfg(test)]
    if let Some(value) = super::options::test_mock::get(pane, test_support::ON_SCREEN) {
        return value == "1";
    }
    window_panes(pane)
        .iter()
        .any(|p| p.pane_id == pane && p.visible)
}

/// Panes whose unread mark should go: unread and on screen.
pub fn panes_to_mark_read(panes: &[WindowPane]) -> Vec<&str> {
    panes
        .iter()
        .filter(|p| p.visible && p.unread)
        .map(|p| p.pane_id.as_str())
        .collect()
}

/// Clear the unread mark of every pane on screen in `window`. Called by
/// the focus hooks, which fire after the window or pane changed, so the
/// check runs against what the client shows now. A window that was only
/// passed through on the way elsewhere is no longer on screen and keeps
/// its marks.
pub fn mark_window_read(window: &str) {
    let panes = window_panes(window);
    let commands: Vec<Vec<&str>> = panes_to_mark_read(&panes)
        .into_iter()
        .map(|pane| vec!["set", "-t", pane, "-p", "-u", PANE_UNREAD])
        .collect();
    if commands.is_empty() {
        return;
    }
    let _ = run_tmux_batch(&commands);
    // The select hook's refresh may have read the mark before it was
    // cleared; without this the row stays marked until the next tick.
    notify_sidebars();
}

#[cfg(test)]
pub mod test_support {
    /// Mock-store key read by [`super::pane_on_screen`] under the test mock.
    pub const ON_SCREEN: &str = "#on_screen";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(
        id: &str,
        role: &str,
        unread: &str,
        active: &str,
        zoomed: &str,
        clients: &str,
    ) -> String {
        format!("{id}|{role}|{unread}|{active}|{zoomed}|1|1|{clients}")
    }

    #[test]
    fn every_pane_of_a_window_on_screen_counts_as_seen() {
        let out = [
            line("%1", "", "1", "1", "0", "1"),
            line("%2", "", "1", "0", "0", "1"),
        ]
        .join("\n");
        let panes = parse_window_panes(&out);
        assert_eq!(panes_to_mark_read(&panes), vec!["%1", "%2"]);
    }

    #[test]
    fn a_window_no_client_shows_is_not_seen() {
        let out = line("%1", "", "1", "1", "0", "0");
        let panes = parse_window_panes(&out);
        assert!(!panes[0].visible);
        assert!(panes_to_mark_read(&panes).is_empty());
    }

    #[test]
    fn a_zoomed_window_shows_only_its_active_pane() {
        let out = [
            line("%1", "", "1", "1", "1", "1"),
            line("%2", "", "1", "0", "1", "1"),
        ]
        .join("\n");
        let panes = parse_window_panes(&out);
        assert_eq!(panes_to_mark_read(&panes), vec!["%1"]);
    }

    #[test]
    fn sidebars_are_left_out() {
        let out = [
            line("%9", "sidebar", "", "1", "0", "1"),
            line("%1", "", "1", "0", "0", "1"),
        ]
        .join("\n");
        let panes = parse_window_panes(&out);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].pane_id, "%1");
    }

    #[test]
    fn only_sidebars_on_screen_are_notified() {
        let out = [
            "sidebar|101|1|1|1",
            "sidebar|102|0|1|0",
            "|103|1|1|1",
            "sidebar||1|1|1",
            "sidebar|1|1|1|1",
        ]
        .join("\n");
        assert_eq!(parse_visible_sidebar_pids(&out), vec![101]);
    }

    #[test]
    fn old_tmux_notifies_sidebars_of_attached_current_windows() {
        let out = ["sidebar|101|1|2|", "sidebar|102|0|2|", "sidebar|103|1|0|"].join("\n");
        assert_eq!(parse_visible_sidebar_pids(&out), vec![101]);
    }

    #[test]
    fn read_panes_are_not_touched() {
        let out = line("%1", "", "", "1", "0", "1");
        assert!(panes_to_mark_read(&parse_window_panes(&out)).is_empty());
    }

    #[test]
    fn old_tmux_without_active_clients_falls_back_to_attached_current_window() {
        let shown = "%1||1|1|0|1|2|";
        let hidden_window = "%2||1|1|0|0|2|";
        let detached = "%3||1|1|0|1|0|";
        let panes = parse_window_panes(&[shown, hidden_window, detached].join("\n"));
        let visible: Vec<bool> = panes.iter().map(|p| p.visible).collect();
        assert_eq!(visible, vec![true, false, false]);
    }
}
