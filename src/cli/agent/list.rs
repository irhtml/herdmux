//! `agent list`: every agent pane with its status, tag and location.

use super::{EXIT_OK, self_pane, usage_error};
use crate::cli::args::{Args, Spec};
use crate::cli::hook::is_permission_wait_reason;
use crate::process::ProcessSnapshot;
use crate::tmux::{self, PaneLocation};

const LAST_TEXT_WIDTH: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) loc: PaneLocation,
    /// `@pane_agent`, or the agent found in the process tree before its
    /// first hook fired.
    pub(super) agent: String,
    pub(super) is_self: bool,
}

/// One word for what the agent is doing, from the caller's point of view.
pub(super) fn state_label(loc: &PaneLocation, agent: &str) -> &'static str {
    if agent.is_empty() {
        return "-";
    }
    match loc.status.as_str() {
        "running" => "running",
        "waiting" if is_permission_wait_reason(&loc.wait_reason) => "blocked",
        "waiting" => "waiting",
        "idle" => "idle",
        "background" => "background",
        "error" => "error",
        // Agent process found but no hook has fired yet.
        _ => "ready",
    }
}

pub(super) fn rows(
    panes: Vec<PaneLocation>,
    self_pane: &str,
    include_all: bool,
    detect: impl Fn(&PaneLocation) -> Option<String>,
) -> Vec<Row> {
    panes
        .into_iter()
        .filter(|loc| !loc.is_sidebar())
        .filter_map(|loc| {
            let agent = if loc.agent.is_empty() {
                detect(&loc).unwrap_or_default()
            } else {
                loc.agent.clone()
            };
            if agent.is_empty() && !include_all {
                return None;
            }
            let is_self = loc.pane_id == self_pane;
            Some(Row {
                loc,
                agent,
                is_self,
            })
        })
        .collect()
}

fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match path.strip_prefix(&home) {
            Some("") => "~".into(),
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_string(),
        },
        _ => path.to_string(),
    }
}

fn truncate(text: &str, width: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= width {
        return text.to_string();
    }
    let cut: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{cut}…")
}

pub(super) fn render_table(rows: &[Row]) -> String {
    let header = ["PANE", "ADDRESS", "AGENT", "STATE", "TAG", "CWD", "LAST"];
    let mut table: Vec<[String; 7]> = vec![header.map(String::from)];
    for row in rows {
        let loc = &row.loc;
        let pane = if row.is_self {
            format!("{}*", loc.pane_id)
        } else {
            loc.pane_id.clone()
        };
        let or_dash = |s: &str| {
            if s.is_empty() {
                "-".to_string()
            } else {
                s.to_string()
            }
        };
        table.push([
            pane,
            loc.address(),
            or_dash(&row.agent),
            state_label(loc, &row.agent).to_string(),
            or_dash(&loc.desc),
            tilde(loc.effective_cwd()),
            truncate(&loc.prompt, LAST_TEXT_WIDTH),
        ]);
    }
    let widths: Vec<usize> = (0..7)
        .map(|col| {
            table
                .iter()
                .map(|r| r[col].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    table
        .iter()
        .map(|r| {
            let line: Vec<String> = r
                .iter()
                .zip(&widths)
                .map(|(cell, w)| format!("{cell:<w$}"))
                .collect();
            line.join("  ").trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn render_json(rows: &[Row]) -> serde_json::Value {
    rows.iter()
        .map(|row| {
            let loc = &row.loc;
            serde_json::json!({
                "pane_id": loc.pane_id,
                "address": loc.address(),
                "agent": row.agent,
                "state": state_label(loc, &row.agent),
                "status": loc.status,
                "wait_reason": loc.wait_reason,
                "desc": loc.desc,
                "cwd": loc.effective_cwd(),
                "worktree": loc.worktree_name,
                "branch": loc.worktree_branch,
                "session_id": loc.session_id,
                "last_text": loc.prompt,
                "last_text_source": loc.prompt_source,
                "prompt_at_ms": loc.prompt_at,
                "self": row.is_self,
            })
        })
        .collect()
}

pub(super) fn run(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &[],
        switches: &["json", "all"],
    };
    let parsed = match Args::parse(raw, &SPEC) {
        Ok(p) if p.positionals.is_empty() => p,
        Ok(_) => return usage_error("list takes no positional arguments"),
        Err(e) => return usage_error(&e),
    };
    let panes = tmux::query_pane_locations();
    // One `ps` for all panes, and only if some pane might be hosting an
    // agent whose hooks have not fired yet.
    let needs_scan = panes
        .iter()
        .any(|p| p.agent.is_empty() && !tmux::is_shell_command(&p.current_command));
    let snapshot = needs_scan.then(ProcessSnapshot::scan).flatten();
    let detect = |loc: &PaneLocation| {
        if tmux::is_shell_command(&loc.current_command) {
            return None;
        }
        let snapshot = snapshot.as_ref()?;
        crate::worktree::AGENTS
            .iter()
            .find(|a| snapshot.interactive_agent_pid(loc.pane_pid, a).is_some())
            .map(|a| a.to_string())
    };
    let rows = rows(panes, &self_pane(), parsed.has("all"), detect);
    if parsed.has("json") {
        println!("{}", render_json(&rows));
    } else if rows.is_empty() {
        eprintln!("no agent panes found");
    } else {
        println!("{}", render_table(&rows));
    }
    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(id: &str, agent: &str, status: &str, reason: &str) -> PaneLocation {
        PaneLocation {
            pane_id: id.into(),
            session: "main".into(),
            window_index: 1,
            pane_index: id[1..].parse().unwrap(),
            agent: agent.into(),
            status: status.into(),
            wait_reason: reason.into(),
            current_path: "/srv/repo".into(),
            current_command: if agent.is_empty() { "zsh" } else { agent }.into(),
            ..Default::default()
        }
    }

    fn sample() -> Vec<PaneLocation> {
        vec![
            PaneLocation {
                desc: "reviewer".into(),
                prompt: "  looks good, merging  ".into(),
                ..loc("%1", "claude", "idle", "")
            },
            loc("%2", "codex", "waiting", "permission_prompt"),
            loc("%3", "", "", ""),
            PaneLocation {
                current_command: "bun".into(),
                ..loc("%4", "", "", "")
            },
            PaneLocation {
                role: "sidebar".into(),
                ..loc("%5", "", "", "")
            },
        ]
    }

    fn detect_bun(loc: &PaneLocation) -> Option<String> {
        (loc.current_command == "bun").then(|| "codex".to_string())
    }

    #[test]
    fn rows_skip_sidebar_and_shells_unless_all() {
        let ids = |rows: Vec<Row>| rows.into_iter().map(|r| r.loc.pane_id).collect::<Vec<_>>();
        assert_eq!(
            ids(rows(sample(), "%1", false, detect_bun)),
            ["%1", "%2", "%4"]
        );
        assert_eq!(
            ids(rows(sample(), "%1", true, detect_bun)),
            ["%1", "%2", "%3", "%4"]
        );
    }

    #[test]
    fn table_marks_self_and_labels_states() {
        let rows = rows(sample(), "%1", false, detect_bun);
        insta::assert_snapshot!(render_table(&rows), @r"
        PANE  ADDRESS   AGENT   STATE    TAG       CWD        LAST
        %1*   main:1.1  claude  idle     reviewer  /srv/repo  looks good, merging
        %2    main:1.2  codex   blocked  -         /srv/repo
        %4    main:1.4  codex   ready    -         /srv/repo
        ");
    }

    #[test]
    fn json_carries_raw_status_and_self_flag() {
        let rows = rows(sample(), "%2", false, detect_bun);
        let json = render_json(&rows);
        assert_eq!(json[1]["state"], "blocked");
        assert_eq!(json[1]["wait_reason"], "permission_prompt");
        assert_eq!(json[1]["self"], true);
        assert_eq!(json[0]["self"], false);
        assert_eq!(json[2]["agent"], "codex");
        assert_eq!(json[2]["prompt_at_ms"], serde_json::Value::Null);
    }

    #[test]
    fn truncate_counts_chars() {
        assert_eq!(truncate("åäö", 3), "åäö");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
