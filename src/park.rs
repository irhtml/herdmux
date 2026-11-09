//! Parked agents: sessions closed with `herdmux park`, kept so the finder
//! can resume them later.
//!
//! One JSON file per parked session under
//! `${XDG_STATE_HOME:-~/.local/state}/herdmux/parked/`, named by session id.
//! Separate files keep two parks that finish at the same time from losing
//! each other's record, and parking the same session again replaces it.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const VERSION: u64 = 1;

/// Starts the handoff prompt `herdmux park` sends. The sidebar and the
/// transcript digest skip prompts with this prefix, so the handoff does not
/// become the session's latest prompt.
pub const PARK_PROMPT_MARKER: &str = "[herdmux park]";

/// The handoff prompt. It asks for a reply short enough to read in the
/// finder, and no tool calls, so the turn ends quickly.
pub const HANDOFF_PROMPT: &str = "[herdmux park] This session is being paused and will be resumed later. \
Do not use any tools. Reply with a handoff under 80 words, in exactly three lines:\n\
Goal: what this session is for\n\
State: where the work stands\n\
Next: the next step when it resumes";

/// True for the handoff prompt (see [`PARK_PROMPT_MARKER`]).
pub fn is_park_prompt(prompt: &str) -> bool {
    prompt.trim_start().starts_with(PARK_PROMPT_MARKER)
}

/// One parked session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parked {
    pub session_id: String,
    pub agent: String,
    /// Working directory of the agent process.
    pub cwd: String,
    /// Command line the agent was started with, normalized to start with
    /// the agent name (see `process::normalize_agent_argv`).
    pub argv: Vec<String>,
    pub argv_exact: bool,
    /// Allowlisted environment (`process::ENV_ALLOWLIST`).
    pub env: Vec<(String, String)>,
    pub parked_at: u64,
    pub tmux_session: String,
    pub window_name: String,
    /// The pane's `@pane_desc` tag.
    pub desc: String,
    pub pinned: bool,
    /// Claude's session title, if any.
    pub title: String,
    /// First typed prompt of the session.
    pub goal: String,
    pub last_prompt: String,
    pub repo: String,
    pub branch: String,
    /// The agent's handoff reply, or the last reply when it gave none.
    pub handoff: String,
    /// `handoff` is the agent's answer to the handoff prompt.
    pub handoff_from_agent: bool,
}

impl Parked {
    /// Name for a list row: the tag, Claude's title, the goal, the agent.
    pub fn name(&self) -> &str {
        [&self.desc, &self.title, &self.goal]
            .into_iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .unwrap_or(&self.agent)
    }

    /// The `Next:` line of the handoff, or its last non-empty line.
    pub fn next_step(&self) -> Option<&str> {
        let lines = || {
            self.handoff
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
        };
        lines()
            .find_map(|l| {
                l.strip_prefix("Next:")
                    .or_else(|| l.strip_prefix("**Next:**"))
                    .map(str::trim)
            })
            .or_else(|| lines().next_back())
    }

    /// The resume entry `resume::build_command` turns into a command line.
    pub(crate) fn resume_entry(&self) -> crate::resume::Entry {
        crate::resume::Entry {
            session: self.tmux_session.clone(),
            cwd: self.cwd.clone(),
            agent: self.agent.clone(),
            session_id: self.session_id.clone(),
            argv: self.argv.clone(),
            argv_exact: self.argv_exact,
            env: self.env.clone(),
            // Park asks for a handoff or reads a reply first, so the
            // session always had a turn.
            had_turn: true,
            transcript_found: true,
            saved_at: self.parked_at,
            ..Default::default()
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "version": VERSION,
            "session_id": self.session_id,
            "agent": self.agent,
            "cwd": self.cwd,
            "argv": self.argv,
            "argv_exact": self.argv_exact,
            "env": self.env.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
            "parked_at": self.parked_at,
            "tmux_session": self.tmux_session,
            "window_name": self.window_name,
            "desc": self.desc,
            "pinned": self.pinned,
            "title": self.title,
            "goal": self.goal,
            "last_prompt": self.last_prompt,
            "repo": self.repo,
            "branch": self.branch,
            "handoff": self.handoff,
            "handoff_from_agent": self.handoff_from_agent,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        if value.get("version")?.as_u64()? != VERSION {
            return None;
        }
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
        let session_id = text("session_id");
        if session_id.is_empty() {
            return None;
        }
        Some(Self {
            session_id,
            agent: text("agent"),
            cwd: text("cwd"),
            argv: value
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            argv_exact: flag("argv_exact"),
            env: value
                .get("env")
                .and_then(Value::as_array)
                .map(|pairs| {
                    pairs
                        .iter()
                        .filter_map(|p| {
                            let p = p.as_array()?;
                            let key = p.first()?.as_str()?;
                            let value = p.get(1)?.as_str()?;
                            // Re-check on load: resume types these into a
                            // shell, and the file is user-writable state.
                            crate::process::ENV_ALLOWLIST
                                .contains(&key)
                                .then(|| (key.to_string(), value.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            parked_at: value.get("parked_at").and_then(Value::as_u64).unwrap_or(0),
            tmux_session: text("tmux_session"),
            window_name: text("window_name"),
            desc: text("desc"),
            pinned: flag("pinned"),
            title: text("title"),
            goal: text("goal"),
            last_prompt: text("last_prompt"),
            repo: text("repo"),
            branch: text("branch"),
            handoff: text("handoff"),
            handoff_from_agent: flag("handoff_from_agent"),
        })
    }
}

/// `${XDG_STATE_HOME:-~/.local/state}/herdmux/parked`.
pub fn parked_dir() -> Option<PathBuf> {
    Some(crate::resume::state_dir()?.join("parked"))
}

/// File of one session. `None` for an id that is not safe as a file name.
fn record_path(dir: &Path, session_id: &str) -> Option<PathBuf> {
    let safe = !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    safe.then(|| dir.join(format!("{session_id}.json")))
}

/// Write `parked`, replacing an earlier record of the same session.
pub fn save(dir: &Path, parked: &Parked) -> std::io::Result<()> {
    let path = record_path(dir, &parked.session_id).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "unsafe session id")
    })?;
    std::fs::create_dir_all(dir)?;
    let body = serde_json::to_string_pretty(&parked.to_json()).unwrap_or_default();
    crate::fs_util::write_private_atomic(&path, body.as_bytes())
}

pub fn load(dir: &Path, session_id: &str) -> Option<Parked> {
    let content = std::fs::read_to_string(record_path(dir, session_id)?).ok()?;
    Parked::from_json(&serde_json::from_str(&content).ok()?)
}

/// Every parked session, most recently parked first. Unreadable files are
/// skipped.
pub fn load_all(dir: &Path) -> Vec<Parked> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut parked: Vec<Parked> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|c| serde_json::from_str(&c).ok())
        .filter_map(|v| Parked::from_json(&v))
        .collect();
    parked.sort_by(|a, b| {
        b.parked_at
            .cmp(&a.parked_at)
            .then(a.session_id.cmp(&b.session_id))
    });
    parked
}

/// Forget a parked session. Removing one that is already gone is fine.
pub fn remove(dir: &Path, session_id: &str) -> std::io::Result<()> {
    let Some(path) = record_path(dir, session_id) else {
        return Ok(());
    };
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parked(sid: &str, at: u64) -> Parked {
        Parked {
            session_id: sid.into(),
            agent: "claude".into(),
            cwd: "/repo".into(),
            argv: vec!["claude".into(), "--model".into(), "opus".into()],
            argv_exact: true,
            env: vec![("CLAUDE_CONFIG_DIR".into(), "/home/u/.claude-work".into())],
            parked_at: at,
            tmux_session: "main".into(),
            window_name: "api".into(),
            desc: "invoice rounding".into(),
            pinned: true,
            title: "Fix invoice rounding".into(),
            goal: "fix the rounding bug".into(),
            last_prompt: "run the tests".into(),
            repo: "billing".into(),
            branch: "fix/rounding".into(),
            handoff: "Goal: fix rounding\nState: tests pass\nNext: open the PR".into(),
            handoff_from_agent: true,
        }
    }

    #[test]
    fn a_record_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let record = parked("sid-1", 100);
        save(dir.path(), &record).unwrap();
        assert_eq!(load(dir.path(), "sid-1"), Some(record));
    }

    #[test]
    fn two_records_live_side_by_side_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &parked("old", 100)).unwrap();
        save(dir.path(), &parked("new", 200)).unwrap();
        let ids: Vec<String> = load_all(dir.path())
            .into_iter()
            .map(|p| p.session_id)
            .collect();
        assert_eq!(ids, vec!["new", "old"]);
    }

    #[test]
    fn parking_a_session_again_replaces_its_record() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &parked("sid", 100)).unwrap();
        save(dir.path(), &parked("sid", 300)).unwrap();
        let all = load_all(dir.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].parked_at, 300);
    }

    #[test]
    fn remove_forgets_a_record_and_ignores_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &parked("sid", 100)).unwrap();
        remove(dir.path(), "sid").unwrap();
        remove(dir.path(), "sid").unwrap();
        assert!(load_all(dir.path()).is_empty());
    }

    #[test]
    fn loading_drops_variables_outside_the_allowlist() {
        let mut parked = parked("sid-env", 1);
        parked.env = vec![
            ("CLAUDE_CONFIG_DIR".into(), "/home/u/.claude-work".into()),
            ("LD_PRELOAD".into(), "/tmp/evil.so".into()),
        ];
        let back = Parked::from_json(&parked.to_json()).unwrap();
        assert_eq!(
            back.env,
            [(
                "CLAUDE_CONFIG_DIR".to_string(),
                "/home/u/.claude-work".to_string()
            )]
        );
    }

    #[test]
    fn unsafe_session_ids_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(save(dir.path(), &parked("../escape", 1)).is_err());
        assert_eq!(load(dir.path(), "../escape"), None);
    }

    #[test]
    fn name_falls_back_from_tag_to_title_to_goal_to_agent() {
        let mut p = parked("sid", 1);
        assert_eq!(p.name(), "invoice rounding");
        p.desc.clear();
        assert_eq!(p.name(), "Fix invoice rounding");
        p.title.clear();
        assert_eq!(p.name(), "fix the rounding bug");
        p.goal.clear();
        assert_eq!(p.name(), "claude");
    }

    #[test]
    fn next_step_reads_the_next_line_or_the_last_line() {
        let mut p = parked("sid", 1);
        assert_eq!(p.next_step(), Some("open the PR"));
        p.handoff = "**Goal:** a\n**Next:** ship it".into();
        assert_eq!(p.next_step(), Some("ship it"));
        p.handoff = "All done, the PR is merged.\n".into();
        assert_eq!(p.next_step(), Some("All done, the PR is merged."));
        p.handoff.clear();
        assert_eq!(p.next_step(), None);
    }

    #[test]
    fn the_resume_entry_resumes_the_session_with_its_flags() {
        let entry = parked("sid-9", 1).resume_entry();
        assert_eq!(
            crate::resume::build_command(&entry),
            crate::resume::ResumeCommand::Resume(vec![
                "claude".into(),
                "--model".into(),
                "opus".into(),
                "--resume".into(),
                "sid-9".into(),
            ])
        );
    }

    #[test]
    fn park_prompts_are_recognised() {
        assert!(is_park_prompt(HANDOFF_PROMPT));
        assert!(!is_park_prompt("park the car"));
    }
}
