//! `resume.json`: what `resume save` saw, read back by `resume restore`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const VERSION: u64 = 1;

/// One agent pane that can be relaunched after a restore. Locations are
/// stored as separate fields, never as a tmux target string, because a
/// session name may contain characters tmux would parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) session: String,
    pub(crate) window_index: u32,
    pub(crate) pane_index: u32,
    pub(crate) window_panes: u32,
    /// Working directory of the agent process (its launch dir).
    pub(crate) cwd: String,
    pub(crate) agent: String,
    pub(crate) session_id: String,
    /// Original command line, normalized to start with the agent name.
    pub(crate) argv: Vec<String>,
    pub(crate) argv_exact: bool,
    /// Allowlisted variables only (`process::ENV_ALLOWLIST`).
    pub(crate) env: Vec<(String, String)>,
    pub(crate) had_turn: bool,
    pub(crate) transcript_found: bool,
    pub(crate) saved_at: u64,
}

/// A `@pane_desc` tag. tmux-resurrect drops pane options, so tags are
/// carried here for every pane, agent or not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tag {
    pub(crate) session: String,
    pub(crate) window_index: u32,
    pub(crate) pane_index: u32,
    pub(crate) window_panes: u32,
    pub(crate) desc: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResumeState {
    pub(crate) socket_path: String,
    pub(crate) saved_at: u64,
    /// When `resume restore` last acted on this snapshot.
    pub(crate) restored_at: Option<u64>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) tags: Vec<Tag>,
}

/// `${XDG_STATE_HOME:-~/.local/state}/herdmux`.
pub(crate) fn state_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(base.join("herdmux"))
}

/// Moves a state directory left under the pre-rename name
/// (`tmux-agent-sidebar`) to [`state_dir`]. Does nothing once the new
/// directory exists.
pub(crate) fn migrate_legacy_state_dir() {
    let Some(dir) = state_dir() else {
        return;
    };
    let Some(legacy) = dir.parent().map(|base| base.join("tmux-agent-sidebar")) else {
        return;
    };
    if !dir.exists() && legacy.is_dir() {
        let _ = std::fs::rename(legacy, dir);
    }
}

/// One file per tmux server socket, so a scratch server (`tmux -L x`)
/// never overwrites the main server's snapshot.
pub(crate) fn state_path(socket_path: &str) -> Option<PathBuf> {
    let name = Path::new(socket_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "default".into());
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(state_dir()?.join(format!("resume-{safe}.json")))
}

impl ResumeState {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "version": VERSION,
            "socket_path": self.socket_path,
            "saved_at": self.saved_at,
            "restored_at": self.restored_at,
            "entries": self.entries.iter().map(entry_to_json).collect::<Vec<_>>(),
            "tags": self.tags.iter().map(|t| json!({
                "session": t.session,
                "window_index": t.window_index,
                "pane_index": t.pane_index,
                "window_panes": t.window_panes,
                "desc": t.desc,
            })).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        if value.get("version")?.as_u64()? != VERSION {
            return None;
        }
        Some(Self {
            socket_path: str_field(value, "socket_path"),
            saved_at: value.get("saved_at")?.as_u64()?,
            restored_at: value.get("restored_at").and_then(Value::as_u64),
            entries: value
                .get("entries")?
                .as_array()?
                .iter()
                .filter_map(entry_from_json)
                .collect(),
            tags: value
                .get("tags")
                .and_then(Value::as_array)
                .map(|tags| tags.iter().filter_map(tag_from_json).collect())
                .unwrap_or_default(),
        })
    }

    pub(crate) fn load(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        Self::from_json(&serde_json::from_str(&content).ok()?)
    }

    pub(crate) fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = serde_json::to_string_pretty(&self.to_json()).unwrap_or_default();
        crate::fs_util::write_private_atomic(path, body.as_bytes())
    }
}

fn entry_to_json(e: &Entry) -> Value {
    json!({
        "session": e.session,
        "window_index": e.window_index,
        "pane_index": e.pane_index,
        "window_panes": e.window_panes,
        "cwd": e.cwd,
        "agent": e.agent,
        "session_id": e.session_id,
        "argv": e.argv,
        "argv_exact": e.argv_exact,
        "env": e.env.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        "had_turn": e.had_turn,
        "transcript_found": e.transcript_found,
        "saved_at": e.saved_at,
    })
}

fn entry_from_json(v: &Value) -> Option<Entry> {
    Some(Entry {
        session: v.get("session")?.as_str()?.to_string(),
        window_index: u32_field(v, "window_index")?,
        pane_index: u32_field(v, "pane_index")?,
        window_panes: u32_field(v, "window_panes")?,
        cwd: str_field(v, "cwd"),
        agent: v.get("agent")?.as_str()?.to_string(),
        session_id: v.get("session_id")?.as_str()?.to_string(),
        argv: v
            .get("argv")?
            .as_array()?
            .iter()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect(),
        argv_exact: bool_field(v, "argv_exact"),
        env: v
            .get("env")
            .and_then(Value::as_array)
            .map(|pairs| {
                pairs
                    .iter()
                    .filter_map(|p| {
                        let pair = p.as_array()?;
                        let key = pair.first()?.as_str()?;
                        let value = pair.get(1)?.as_str()?;
                        // Re-check on load: the file is user-writable state.
                        crate::process::ENV_ALLOWLIST
                            .contains(&key)
                            .then(|| (key.to_string(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        had_turn: bool_field(v, "had_turn"),
        transcript_found: bool_field(v, "transcript_found"),
        saved_at: v.get("saved_at").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn tag_from_json(v: &Value) -> Option<Tag> {
    Some(Tag {
        session: v.get("session")?.as_str()?.to_string(),
        window_index: u32_field(v, "window_index")?,
        pane_index: u32_field(v, "pane_index")?,
        window_panes: u32_field(v, "window_panes")?,
        desc: v.get("desc")?.as_str()?.to_string(),
    })
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn bool_field(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn u32_field(v: &Value, key: &str) -> Option<u32> {
    v.get(key)?.as_u64()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> Entry {
        Entry {
            session: "fr24".into(),
            window_index: 15,
            pane_index: 2,
            window_panes: 4,
            cwd: "/home/u/www/fr24/agent-loop".into(),
            agent: "claude".into(),
            session_id: "c0ffee".into(),
            argv: vec!["claude".into(), "--chrome".into()],
            argv_exact: true,
            env: vec![("CLAUDE_CONFIG_DIR".into(), "/home/u/.claude-fr24".into())],
            had_turn: true,
            transcript_found: true,
            saved_at: 1_790_000_000,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let state = ResumeState {
            socket_path: "/tmp/tmux-1000/default".into(),
            saved_at: 1_790_000_000,
            restored_at: Some(1_790_000_100),
            entries: vec![sample_entry()],
            tags: vec![Tag {
                session: "fr24".into(),
                window_index: 1,
                pane_index: 0,
                window_panes: 3,
                desc: "review | bookmarks".into(),
            }],
        };
        assert_eq!(ResumeState::from_json(&state.to_json()), Some(state));
    }

    #[test]
    fn rejects_other_versions_and_drops_unknown_env() {
        let mut json = ResumeState {
            entries: vec![sample_entry()],
            ..Default::default()
        }
        .to_json();
        json["entries"][0]["env"] =
            serde_json::json!([["ANTHROPIC_API_KEY", "x"], ["CODEX_HOME", "/c"]]);
        let state = ResumeState::from_json(&json).unwrap();
        assert_eq!(
            state.entries[0].env,
            vec![("CODEX_HOME".to_string(), "/c".to_string())]
        );
        json["version"] = 99.into();
        assert_eq!(ResumeState::from_json(&json), None);
    }

    #[test]
    fn state_path_is_per_socket() {
        let a = state_path("/tmp/tmux-1000/default").unwrap();
        let b = state_path("/tmp/tmux-1000/restest").unwrap();
        assert!(a.ends_with("herdmux/resume-default.json"));
        assert!(b.ends_with("herdmux/resume-restest.json"));
        assert!(
            state_path("/tmp/odd name/s.ock")
                .unwrap()
                .ends_with("resume-s_ock.json")
        );
    }

    #[test]
    fn write_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/resume.json");
        let state = ResumeState {
            socket_path: "/s".into(),
            saved_at: 5,
            restored_at: None,
            entries: vec![sample_entry()],
            tags: vec![],
        };
        state.write(&path).unwrap();
        assert_eq!(ResumeState::load(&path), Some(state));
    }
}
