//! `resume save`: record the agent panes worth relaunching after a restore.

use std::path::PathBuf;

use super::argv::{self, ResumeCommand};
use super::resurrect::{Snapshot, same_dir};
use super::store::{Entry, ResumeState, Tag};
use crate::process::{Invocation, ProcessSnapshot, normalize_agent_argv, read_invocation};
use crate::tmux::PaneLocation;

/// How long a relaunched pane that has not reported in yet keeps its
/// previous entry. Covers a save that lands between the restore and the
/// agent's SessionStart.
const PENDING_GRACE_SECS: u64 = 30 * 60;

pub(crate) trait SaveEnv {
    /// The agent's exact invocation and working directory, if it is the
    /// pane's interactive program.
    fn interactive_agent(&self, loc: &PaneLocation) -> Option<(Invocation, String)>;
    fn transcript_exists(&self, agent: &str, session_id: &str, env: &[(String, String)]) -> bool;
}

#[derive(Debug, Default)]
pub(crate) struct SaveReport {
    pub(crate) state: ResumeState,
    /// `(address, reason)` for agent panes left out.
    pub(crate) skipped: Vec<(String, String)>,
    /// Entries kept from the previous save because their relaunch is
    /// still pending.
    pub(crate) carried: usize,
}

fn had_turn(loc: &PaneLocation) -> bool {
    !loc.prompt.is_empty()
        || loc.prompt_at.is_some()
        || loc.wait_reason.starts_with("session_resumed")
}

/// Problem with the pane's resurrect row, if any. Resurrect restores
/// panes by position, so an entry is only trustworthy when the snapshot
/// shows the same pane at the same place.
fn resurrect_mismatch(loc: &PaneLocation, snapshot: &Snapshot) -> Option<&'static str> {
    let key = (loc.session.clone(), loc.window_index, loc.pane_index);
    let Some(row) = snapshot.panes.get(&key) else {
        return Some("not in the resurrect snapshot");
    };
    if !same_dir(&row.dir, &loc.current_path) {
        return Some("resurrect saved a different directory");
    }
    if row.command != loc.current_command {
        return Some("resurrect saved a different command");
    }
    let window_panes = snapshot
        .window_panes
        .get(&(loc.session.clone(), loc.window_index))
        .copied();
    if window_panes != Some(loc.window_panes) {
        return Some("window layout changed during the save");
    }
    None
}

pub(crate) fn collect<E: SaveEnv>(
    env: &E,
    panes: &[PaneLocation],
    snapshot: Option<&Snapshot>,
    previous: Option<&ResumeState>,
    socket_path: &str,
    now: u64,
) -> SaveReport {
    let mut report = SaveReport::default();
    report.state.socket_path = socket_path.to_string();
    report.state.saved_at = now;

    for loc in panes.iter().filter(|p| !p.is_sidebar()) {
        if !loc.desc.is_empty() {
            report.state.tags.push(Tag {
                session: loc.session.clone(),
                window_index: loc.window_index,
                pane_index: loc.pane_index,
                window_panes: loc.window_panes,
                desc: loc.desc.clone(),
            });
        }
        if loc.agent.is_empty() {
            continue;
        }
        let mut skip = |reason: &str| report.skipped.push((loc.address(), reason.to_string()));
        if loc.session_id.is_empty() {
            skip("no session id reported yet");
            continue;
        }
        let Some((invocation, cwd)) = env.interactive_agent(loc) else {
            skip("agent is not the pane's interactive program");
            continue;
        };
        if let Some(reason) = snapshot.and_then(|s| resurrect_mismatch(loc, s)) {
            skip(reason);
            continue;
        }
        let transcript_found = env.transcript_exists(&loc.agent, &loc.session_id, &invocation.env);
        let entry = Entry {
            session: loc.session.clone(),
            window_index: loc.window_index,
            pane_index: loc.pane_index,
            window_panes: loc.window_panes,
            cwd,
            agent: loc.agent.clone(),
            session_id: loc.session_id.clone(),
            argv: normalize_agent_argv(&invocation.argv, &loc.agent),
            argv_exact: invocation.argv_exact,
            env: invocation.env,
            had_turn: had_turn(loc),
            transcript_found,
            saved_at: now,
        };
        if let ResumeCommand::Skip(reason) = argv::build(&entry) {
            skip(&reason);
            continue;
        }
        report.state.entries.push(entry);
    }

    if let Some(previous) = previous {
        for old in &previous.entries {
            let taken = report.state.entries.iter().any(|e| {
                (&e.session, e.window_index, e.pane_index)
                    == (&old.session, old.window_index, old.pane_index)
            });
            let pending = panes.iter().any(|p| {
                (&p.session, p.window_index, p.pane_index)
                    == (&old.session, old.window_index, old.pane_index)
                    && p.resume_pending
                        .parse::<u64>()
                        .is_ok_and(|at| now.saturating_sub(at) < PENDING_GRACE_SECS)
            });
            if !taken && pending {
                report.state.entries.push(old.clone());
                report.carried += 1;
            }
        }
    }
    report
}

pub(crate) struct RealSaveEnv {
    pub(crate) processes: Option<ProcessSnapshot>,
}

impl SaveEnv for RealSaveEnv {
    fn interactive_agent(&self, loc: &PaneLocation) -> Option<(Invocation, String)> {
        let pid = self
            .processes
            .as_ref()?
            .interactive_agent_pid(loc.pane_pid, &loc.agent)?;
        let invocation = read_invocation(pid)?;
        let cwd = crate::process::process_cwd(pid).unwrap_or_else(|| loc.current_path.clone());
        Some((invocation, cwd))
    }

    fn transcript_exists(&self, agent: &str, session_id: &str, env: &[(String, String)]) -> bool {
        agent == "claude" && claude_transcript_exists(session_id, env)
    }
}

/// `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/*/<session_id>.jsonl`.
fn claude_transcript_exists(session_id: &str, env: &[(String, String)]) -> bool {
    let safe_id = !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !safe_id {
        return false;
    }
    let config_dir = env
        .iter()
        .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
        .map(|(_, v)| PathBuf::from(v))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")));
    let Some(projects) = config_dir.map(|d| d.join("projects")) else {
        return false;
    };
    let file = format!("{session_id}.jsonl");
    std::fs::read_dir(projects)
        .map(|dirs| dirs.flatten().any(|d| d.path().join(&file).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeEnv {
        agents: HashMap<String, (Invocation, String)>,
        transcripts: Vec<String>,
    }

    impl SaveEnv for FakeEnv {
        fn interactive_agent(&self, loc: &PaneLocation) -> Option<(Invocation, String)> {
            self.agents.get(&loc.pane_id).cloned()
        }
        fn transcript_exists(&self, _agent: &str, sid: &str, _env: &[(String, String)]) -> bool {
            self.transcripts.iter().any(|t| t == sid)
        }
    }

    fn inv(cmd: &str) -> Invocation {
        Invocation {
            argv: cmd.split(' ').map(str::to_string).collect(),
            env: vec![],
            argv_exact: true,
        }
    }

    fn pane(id: &str, index: u32, agent: &str, sid: &str) -> PaneLocation {
        PaneLocation {
            pane_id: id.into(),
            session: "main".into(),
            window_index: 1,
            pane_index: index,
            window_panes: 3,
            current_path: "/repo".into(),
            current_command: if agent.is_empty() { "zsh" } else { agent }.into(),
            agent: agent.into(),
            session_id: sid.into(),
            prompt: if sid.is_empty() { "" } else { "hi" }.into(),
            ..Default::default()
        }
    }

    fn env() -> FakeEnv {
        FakeEnv {
            agents: HashMap::from([
                (
                    "%1".to_string(),
                    (inv("claude --chrome"), "/repo".to_string()),
                ),
                (
                    "%3".to_string(),
                    (inv("claude -p job"), "/repo".to_string()),
                ),
            ]),
            transcripts: vec!["s1".into()],
        }
    }

    #[test]
    fn records_interactive_agents_and_tags() {
        let mut tagged_shell = pane("%0", 0, "", "");
        tagged_shell.desc = "notes".into();
        let panes = vec![
            tagged_shell,
            pane("%1", 1, "claude", "s1"),
            // Agent nested in a script: no interactive invocation.
            pane("%2", 2, "claude", "s2"),
        ];
        let report = collect(&env(), &panes, None, None, "/sock", 100);
        assert_eq!(report.state.entries.len(), 1);
        let entry = &report.state.entries[0];
        assert_eq!(entry.argv, vec!["claude", "--chrome"]);
        assert!(entry.had_turn && entry.transcript_found);
        assert_eq!(report.state.tags.len(), 1);
        assert_eq!(report.state.tags[0].desc, "notes");
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, "main:1.2");
    }

    #[test]
    fn skips_print_mode_and_missing_session_ids() {
        let panes = vec![pane("%3", 3, "claude", "s3"), pane("%1", 1, "claude", "")];
        let report = collect(&env(), &panes, None, None, "/sock", 100);
        assert!(report.state.entries.is_empty());
        assert_eq!(report.skipped.len(), 2);
    }

    #[test]
    fn resurrect_snapshot_must_agree_on_the_pane() {
        let panes = vec![pane("%1", 1, "claude", "s1")];
        let good = Snapshot::parse(
            "pane\tmain\t1\t1\t:*\t0\tt\t:/repo\t0\tzsh\t:\n\
             pane\tmain\t1\t1\t:*\t1\tt\t:/repo\t1\tclaude\t:claude --chrome\n\
             pane\tmain\t1\t1\t:*\t2\tt\t:/repo\t0\tzsh\t:\n",
        );
        assert_eq!(
            collect(&env(), &panes, Some(&good), None, "/s", 1)
                .state
                .entries
                .len(),
            1
        );
        let moved = Snapshot::parse(
            "pane\tmain\t1\t1\t:*\t1\tt\t:/elsewhere\t1\tclaude\t:\n\
             pane\tmain\t1\t1\t:*\t2\tt\t:/repo\t0\tzsh\t:\n\
             pane\tmain\t1\t1\t:*\t3\tt\t:/repo\t0\tzsh\t:\n",
        );
        let report = collect(&env(), &panes, Some(&moved), None, "/s", 1);
        assert!(report.state.entries.is_empty());
        let fewer = Snapshot::parse("pane\tmain\t1\t1\t:*\t1\tt\t:/repo\t1\tclaude\t:\n");
        let report = collect(&env(), &panes, Some(&fewer), None, "/s", 1);
        assert_eq!(report.skipped[0].1, "window layout changed during the save");
    }

    #[test]
    fn pending_relaunch_keeps_its_previous_entry() {
        let previous = ResumeState {
            entries: vec![Entry {
                session: "main".into(),
                window_index: 1,
                pane_index: 2,
                agent: "claude".into(),
                session_id: "old".into(),
                argv: vec!["claude".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut relaunched = pane("%9", 2, "", "");
        relaunched.resume_pending = "1000".into();
        let report = collect(
            &env(),
            std::slice::from_ref(&relaunched),
            None,
            Some(&previous),
            "/s",
            1000 + 60,
        );
        assert_eq!(report.carried, 1);
        assert_eq!(report.state.entries[0].session_id, "old");

        let stale = collect(
            &env(),
            &[relaunched],
            None,
            Some(&previous),
            "/s",
            1000 + PENDING_GRACE_SECS + 1,
        );
        assert_eq!(stale.carried, 0);
    }

    #[test]
    fn transcript_lookup_honors_config_dir_and_rejects_odd_ids() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("projects/-home-u-repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("abc-123.jsonl"), "{}").unwrap();
        let env = vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.path().to_string_lossy().into_owned(),
        )];
        assert!(claude_transcript_exists("abc-123", &env));
        assert!(!claude_transcript_exists("nope", &env));
        assert!(!claude_transcript_exists("../abc-123", &env));
    }
}
