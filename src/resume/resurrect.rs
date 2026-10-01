//! Reader for tmux-resurrect snapshot files, used to cross-check that the
//! panes `resume save` records sit where resurrect will restore them.
//!
//! Pane rows are tab separated:
//! `pane, session, window_index, window_active, :flags, pane_index,
//!  title, :dir, pane_active, command, :full_command`.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneRow {
    pub(crate) dir: String,
    pub(crate) command: String,
}

/// Pane rows keyed by `(session, window_index, pane_index)`, plus the
/// pane count of every window.
#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) panes: HashMap<(String, u32, u32), PaneRow>,
    pub(crate) window_panes: HashMap<(String, u32), u32>,
}

impl Snapshot {
    pub(crate) fn parse(content: &str) -> Self {
        let mut snapshot = Snapshot::default();
        for line in content.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 11 || f[0] != "pane" {
                continue;
            }
            let (Ok(window), Ok(pane)) = (f[2].parse::<u32>(), f[5].parse::<u32>()) else {
                continue;
            };
            let session = f[1].to_string();
            *snapshot
                .window_panes
                .entry((session.clone(), window))
                .or_default() += 1;
            snapshot.panes.insert(
                (session, window, pane),
                PaneRow {
                    dir: unescape_dir(f[7]),
                    command: f[9].to_string(),
                },
            );
        }
        snapshot
    }
}

/// Resurrect prefixes the dir with `:` and backslash-escapes a space.
fn unescape_dir(raw: &str) -> String {
    raw.strip_prefix(':').unwrap_or(raw).replace("\\ ", " ")
}

/// Compare paths the way resurrect stores them: it collapses runs of
/// whitespace (`echo $dir`), so only whitespace-normalized forms match.
pub(crate) fn same_dir(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "pane\tfr24\t15\t1\t:*\t1\ttitle\t:/home/u/www/fr24/agent-loop\t0\tzsh\t:\n\
pane\tfr24\t15\t1\t:*\t2\ttitle\t:/home/u/www/fr24/agent-loop\t1\tclaude\t:claude --chrome --resume\n\
pane\tmain\t3\t0\t:-\t1\tt\t:/home/u/my\\ docs\t1\tzsh\t:\n\
window\tfr24\t15\t:zsh\t1\t:*\tlayout\t:\n\
state\tfr24\tmain\n\
pane\tbroken\n";

    #[test]
    fn parses_pane_rows_and_window_counts() {
        let snap = Snapshot::parse(SAMPLE);
        assert_eq!(snap.panes.len(), 3);
        let row = &snap.panes[&("fr24".to_string(), 15, 2)];
        assert_eq!(row.command, "claude");
        assert_eq!(row.dir, "/home/u/www/fr24/agent-loop");
        assert_eq!(snap.window_panes[&("fr24".to_string(), 15)], 2);
        assert_eq!(
            snap.panes[&("main".to_string(), 3, 1)].dir,
            "/home/u/my docs"
        );
    }

    #[test]
    fn same_dir_tolerates_resurrect_whitespace_collapsing() {
        assert!(same_dir("/a/b  c", "/a/b c"));
        assert!(!same_dir("/a/b", "/a/c"));
    }
}
