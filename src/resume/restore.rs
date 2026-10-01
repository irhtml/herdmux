//! `resume restore`: relaunch saved agents in the panes tmux-resurrect
//! just recreated. Planning is pure; only `execute` touches tmux.

use std::time::Duration;

use super::argv::{self, ResumeCommand};
use super::resurrect::same_dir;
use super::store::ResumeState;
use crate::tmux::{self, PaneLocation};

/// Restores only run this soon after the tmux server started, so a stale
/// snapshot can never type commands into a long-running session.
pub(crate) const RESTORE_WINDOW_SECS: u64 = 10 * 60;
const STAGGER: Duration = Duration::from_millis(300);
const SHELL_READY_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerInfo {
    pub(crate) socket_path: String,
    pub(crate) start_time: u64,
}

impl ServerInfo {
    pub(crate) fn query() -> Option<Self> {
        let out =
            tmux::run_tmux_capture(&["display-message", "-p", "#{socket_path}\t#{start_time}"])
                .ok()?;
        let (socket_path, start) = out.split_once('\t')?;
        Some(Self {
            socket_path: socket_path.to_string(),
            start_time: start.trim().parse().ok()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Launch {
        pane_id: String,
        address: String,
        command: String,
        resumed: bool,
    },
    Skip {
        address: String,
        reason: String,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) actions: Vec<Action>,
    /// `(pane_id, desc)` tags to put back.
    pub(crate) tags: Vec<(String, String)>,
}

impl Plan {
    pub(crate) fn launches(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, Action::Launch { .. }))
            .count()
    }
}

fn find<'a>(
    live: &'a [PaneLocation],
    session: &str,
    window: u32,
    pane: u32,
) -> Option<&'a PaneLocation> {
    live.iter()
        .find(|l| l.session == session && l.window_index == window && l.pane_index == pane)
}

pub(crate) fn plan(
    state: &ResumeState,
    live: &[PaneLocation],
    server: &ServerInfo,
    now: u64,
    force: bool,
) -> Result<Plan, String> {
    if !force {
        if state.socket_path != server.socket_path {
            return Err(format!(
                "the snapshot is from tmux server {}, this is {}",
                state.socket_path, server.socket_path
            ));
        }
        let uptime = now.saturating_sub(server.start_time);
        if uptime > RESTORE_WINDOW_SECS {
            return Err(format!(
                "the tmux server has been up {} min; restore only runs right after a \
                 server start (use --force to run it anyway)",
                uptime / 60
            ));
        }
    }

    let mut plan = Plan::default();
    for entry in &state.entries {
        let address = format!(
            "{}:{}.{}",
            entry.session, entry.window_index, entry.pane_index
        );
        let skip = |reason: String| Action::Skip {
            address: address.clone(),
            reason,
        };
        let Some(loc) = find(live, &entry.session, entry.window_index, entry.pane_index) else {
            plan.actions.push(skip("pane was not restored".into()));
            continue;
        };
        let action = if !loc.agent.is_empty() {
            skip(format!("{} is already running there", loc.agent))
        } else if !tmux::is_shell_command(&loc.current_command) {
            skip(format!("pane is running {}", loc.current_command))
        } else if loc.window_panes != entry.window_panes {
            skip("window layout differs from the snapshot".into())
        } else if !same_dir(&loc.current_path, &entry.cwd) {
            skip(format!(
                "pane is in {} instead of {}",
                loc.current_path, entry.cwd
            ))
        } else {
            match argv::build(entry) {
                ResumeCommand::Resume(argv) | ResumeCommand::Fresh(argv) if argv.is_empty() => {
                    skip("empty command".into())
                }
                ResumeCommand::Resume(argv) => Action::Launch {
                    pane_id: loc.pane_id.clone(),
                    address: address.clone(),
                    command: argv::render(&entry.env, &argv),
                    resumed: true,
                },
                ResumeCommand::Fresh(argv) => Action::Launch {
                    pane_id: loc.pane_id.clone(),
                    address: address.clone(),
                    command: argv::render(&entry.env, &argv),
                    resumed: false,
                },
                ResumeCommand::Skip(reason) => skip(reason),
            }
        };
        plan.actions.push(action);
    }

    for tag in &state.tags {
        if let Some(loc) = find(live, &tag.session, tag.window_index, tag.pane_index)
            && loc.window_panes == tag.window_panes
            && loc.desc.is_empty()
        {
            plan.tags.push((loc.pane_id.clone(), tag.desc.clone()));
        }
    }
    Ok(plan)
}

/// Run the plan against tmux. Returns one log line per action.
pub(crate) fn execute(plan: &Plan, now: u64) -> Vec<String> {
    let mut log = Vec::new();
    for (pane, desc) in &plan.tags {
        tmux::set_pane_option(pane, tmux::PANE_DESC, desc);
    }
    for action in &plan.actions {
        match action {
            Action::Launch {
                pane_id,
                address,
                command,
                resumed,
            } => {
                wait_for_shell(pane_id);
                tmux::set_pane_option(pane_id, tmux::PANE_RESUME_PENDING, &now.to_string());
                let verb = if *resumed { "resume" } else { "fresh" };
                match tmux::send_command(pane_id, command) {
                    Ok(()) => log.push(format!("{verb} {address} {pane_id}: {command}")),
                    Err(e) => {
                        tmux::unset_pane_option(pane_id, tmux::PANE_RESUME_PENDING);
                        log.push(format!("failed {address} {pane_id}: {e}"));
                    }
                }
                std::thread::sleep(STAGGER);
            }
            Action::Skip { address, reason } => log.push(format!("skip {address}: {reason}")),
        }
    }
    log
}

/// Give a freshly restored shell a moment to draw its prompt, so the
/// typed command is not swallowed by its startup.
fn wait_for_shell(pane: &str) {
    let start = std::time::Instant::now();
    while start.elapsed() < SHELL_READY_LIMIT {
        if tmux::capture_pane(pane, 5).is_ok_and(|s| !s.trim().is_empty()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::{Entry, Tag};
    use super::*;

    fn server() -> ServerInfo {
        ServerInfo {
            socket_path: "/tmp/tmux-1000/default".into(),
            start_time: 1_000,
        }
    }

    fn entry(pane: u32) -> Entry {
        Entry {
            session: "main".into(),
            window_index: 1,
            pane_index: pane,
            window_panes: 3,
            cwd: "/repo".into(),
            agent: "claude".into(),
            session_id: format!("sid-{pane}"),
            argv: vec!["claude".into(), "--chrome".into()],
            argv_exact: true,
            env: vec![("CLAUDE_CONFIG_DIR".into(), "/home/u/.claude-x".into())],
            had_turn: true,
            transcript_found: true,
            saved_at: 900,
        }
    }

    fn restored(pane: u32) -> PaneLocation {
        PaneLocation {
            pane_id: format!("%{}", 10 + pane),
            session: "main".into(),
            window_index: 1,
            pane_index: pane,
            window_panes: 3,
            current_path: "/repo".into(),
            current_command: "zsh".into(),
            ..Default::default()
        }
    }

    fn state(entries: Vec<Entry>) -> ResumeState {
        ResumeState {
            socket_path: server().socket_path,
            saved_at: 900,
            restored_at: None,
            entries,
            tags: vec![],
        }
    }

    #[test]
    fn launches_resume_commands_with_saved_env() {
        let plan = plan(
            &state(vec![entry(1)]),
            &[restored(1)],
            &server(),
            1_060,
            false,
        )
        .unwrap();
        assert_eq!(
            plan.actions,
            vec![Action::Launch {
                pane_id: "%11".into(),
                address: "main:1.1".into(),
                command: "CLAUDE_CONFIG_DIR=/home/u/.claude-x claude --chrome --resume sid-1"
                    .into(),
                resumed: true,
            }]
        );
    }

    #[test]
    fn refuses_stale_servers_and_other_sockets_unless_forced() {
        let s = state(vec![entry(1)]);
        assert!(
            plan(
                &s,
                &[restored(1)],
                &server(),
                1_000 + RESTORE_WINDOW_SECS + 1,
                false
            )
            .is_err()
        );
        let other = ServerInfo {
            socket_path: "/tmp/tmux-1000/other".into(),
            ..server()
        };
        assert!(plan(&s, &[restored(1)], &other, 1_010, false).is_err());
        assert_eq!(
            plan(&s, &[restored(1)], &other, 99_999, true)
                .unwrap()
                .launches(),
            1
        );
    }

    #[test]
    fn skips_panes_that_do_not_look_restored() {
        let mut busy = restored(2);
        busy.current_command = "vim".into();
        let mut running = restored(3);
        running.agent = "claude".into();
        let mut moved = restored(4);
        moved.current_path = "/elsewhere".into();
        let mut resized = restored(5);
        resized.window_panes = 2;
        let live = vec![busy, running, moved, resized];
        let entries = (1..=5).map(entry).collect();
        let plan = plan(&state(entries), &live, &server(), 1_010, false).unwrap();
        let reasons: Vec<String> = plan
            .actions
            .iter()
            .map(|a| match a {
                Action::Skip { reason, .. } => reason.clone(),
                Action::Launch { address, .. } => format!("launch {address}"),
            })
            .collect();
        assert_eq!(
            reasons,
            vec![
                "pane was not restored",
                "pane is running vim",
                "claude is already running there",
                "pane is in /elsewhere instead of /repo",
                "window layout differs from the snapshot",
            ]
        );
    }

    #[test]
    fn restores_tags_only_onto_untagged_panes_in_matching_windows() {
        let mut tagged = restored(2);
        tagged.desc = "mine".into();
        let live = vec![restored(1), tagged, restored(0)];
        let tag = |pane, panes| Tag {
            session: "main".into(),
            window_index: 1,
            pane_index: pane,
            window_panes: panes,
            desc: format!("tag-{pane}"),
        };
        let s = ResumeState {
            tags: vec![tag(1, 3), tag(2, 3), tag(0, 4)],
            ..state(vec![])
        };
        let plan = plan(&s, &live, &server(), 1_010, false).unwrap();
        assert_eq!(plan.tags, vec![("%11".to_string(), "tag-1".to_string())]);
    }
}
