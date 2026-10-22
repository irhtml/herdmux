//! Reads Claude Code transcripts to recover what each session is about:
//! its AI title, the prompt that started it (its goal), and every prompt
//! the user typed, which the sidebar search matches against.
//!
//! A transcript lives at `<config dir>/projects/<cwd slug>/<session id>.jsonl`
//! and only grows, so [`TranscriptTail`] reads each file once and then only
//! the bytes appended since the last poll.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Prompts kept per session. The first prompt (the goal) is always kept;
/// past the cap the oldest of the others is dropped.
const MAX_PROMPTS: usize = 400;
/// Longest prompt kept, in characters. Longer ones are cut.
const MAX_PROMPT_CHARS: usize = 2000;

/// What the sidebar knows about one Claude session from its transcript.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionDigest {
    /// Latest title Claude Code generated for the session (`ai-title`).
    pub title: String,
    /// Prompts the user typed, oldest first, with whitespace collapsed.
    /// Shared so cloning a digest for the UI thread stays cheap.
    pub prompts: Arc<Vec<String>>,
    /// Newest message timestamp, in epoch seconds. Unlike the file's
    /// modification time, resuming a session does not move it.
    pub last_event_at: Option<u64>,
}

impl SessionDigest {
    /// The prompt that started the session.
    pub fn goal(&self) -> Option<&str> {
        self.prompts.first().map(String::as_str)
    }

    /// The most recent prompt.
    pub fn last_prompt(&self) -> Option<&str> {
        self.prompts.last().map(String::as_str)
    }

    fn push_prompt(&mut self, prompt: String) {
        let prompts = Arc::make_mut(&mut self.prompts);
        if prompts.len() >= MAX_PROMPTS {
            prompts.remove(1);
        }
        prompts.push(prompt);
    }
}

/// One transcript line the digest cares about.
#[derive(Debug, PartialEq)]
enum Entry {
    Prompt(String),
    Title(String),
}

/// Epoch seconds of the last `"timestamp":"…"` in a line, if any.
fn line_timestamp(line: &str) -> Option<u64> {
    const KEY: &str = "\"timestamp\":\"";
    let start = line.rfind(KEY)? + KEY.len();
    parse_utc_timestamp(line.get(start..start + 19)?)
}

/// Parse the `YYYY-MM-DDTHH:MM:SS` prefix of a UTC timestamp.
fn parse_utc_timestamp(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| s.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    u64::try_from(secs).ok()
}

/// Parse one transcript line. Returns `None` for everything except prompts
/// the user typed and AI titles.
fn parse_line(line: &str) -> Option<Entry> {
    // Quotes inside JSON strings are escaped, so these raw substrings only
    // appear as structure. The checks skip the JSON parse for the many
    // assistant, tool-result and bookkeeping lines.
    if line.contains("\"type\":\"ai-title\"") {
        let value: Value = serde_json::from_str(line).ok()?;
        if value.get("type")?.as_str()? != "ai-title" {
            return None;
        }
        let title = value.get("aiTitle")?.as_str()?.trim();
        return (!title.is_empty()).then(|| Entry::Title(title.to_string()));
    }
    if !line.contains("\"type\":\"user\"") || line.contains("\"type\":\"tool_result\"") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "user" {
        return None;
    }
    let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
    if flag("isMeta") || flag("isSidechain") {
        return None;
    }
    // Newer Claude Code versions mark where a user message came from. Only
    // `human` is typed by the user; task notifications, messages from other
    // sessions and scheduled wake-ups are not. `sdk` is kept: a headless
    // session's prompt is all there is to say what it does.
    let text_field = |key: &str| value.get(key).and_then(Value::as_str);
    let kind = value
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str);
    if kind.is_some_and(|k| k != "human")
        || text_field("turnOrigin").is_some_and(|t| t != "human" && t != "sdk")
        || text_field("promptSource") == Some("system")
    {
        return None;
    }
    let content = value.get("message")?.get("content")?;
    let raw = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    clean_prompt(&raw).map(Entry::Prompt)
}

/// Text Claude Code injects as a user message without the user typing it.
const INJECTED_PREFIXES: &[&str] = &[
    "<system-reminder>",
    "<task-notification>",
    "<local-command-",
    "<bash-stdout>",
    "<bash-stderr>",
    "<command-message>",
    "<user-memory-input>",
    "[Request interrupted by user",
    "This session is being continued from a previous conversation",
    "Caveat: The messages below were generated",
];

/// Commands that say nothing about what a session is for.
const NOISE_COMMANDS: &[&str] = &[
    "/clear",
    "/compact",
    "/resume",
    "/exit",
    "/quit",
    "/model",
    "/config",
    "/status",
    "/cost",
    "/help",
    "/login",
    "/logout",
    "/doctor",
    "/memory",
    "/permissions",
    "/hooks",
    "/mcp",
    "/agents",
    "/plugin",
    "/rewind",
    "/usage",
    "/context",
    "/export",
    "/fast",
];

/// Turn a raw user message into the prompt to show and search, or `None`
/// when the user did not type it.
fn clean_prompt(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let shown = if let Some(name) = tag_body(text, "command-name") {
        let args = tag_body(text, "command-args").unwrap_or("").trim();
        if args.is_empty() || NOISE_COMMANDS.contains(&name.trim()) {
            return None;
        }
        format!("{} {}", name.trim(), args)
    } else if let Some(cmd) = tag_body(text, "bash-input") {
        format!("!{}", cmd.trim())
    } else if INJECTED_PREFIXES.iter().any(|p| text.starts_with(p))
        || NOISE_COMMANDS.contains(&text.split_whitespace().next().unwrap_or(""))
    {
        return None;
    } else {
        text.to_string()
    };
    let collapsed = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(match collapsed.char_indices().nth(MAX_PROMPT_CHARS) {
        Some((cut, _)) => format!("{}…", &collapsed[..cut]),
        None => collapsed,
    })
}

/// Body of the first `<tag>…</tag>` in `text`, when `text` starts with a tag.
fn tag_body<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    if !text.starts_with('<') {
        return None;
    }
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(&text[start..end])
}

/// Claude config directories to look for transcripts in:
/// `$CLAUDE_CONFIG_DIR`, `~/.claude`, and any `~/.claude-*` directory (a
/// common way to keep separate accounts apart).
pub fn config_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if dir.join("projects").is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        push(PathBuf::from(dir));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        push(home.join(".claude"));
        if let Ok(entries) = std::fs::read_dir(&home) {
            let mut extra: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(".claude-"))
                .map(|e| e.path())
                .collect();
            extra.sort();
            for dir in extra {
                push(dir);
            }
        }
    }
    dirs
}

/// Project directory name Claude Code uses for a working directory: every
/// character that is not ASCII alphanumeric becomes `-`.
fn project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Find `<dir>/projects/*/<session_id>.jsonl` in `dirs`, trying the
/// project of `cwd_hint` first.
pub fn find_transcript(session_id: &str, cwd_hint: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let safe_id = !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !safe_id {
        return None;
    }
    let file = format!("{session_id}.jsonl");
    if !cwd_hint.is_empty() {
        let slug = project_slug(cwd_hint);
        for dir in dirs {
            let path = dir.join("projects").join(&slug).join(&file);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    for dir in dirs {
        let Ok(projects) = std::fs::read_dir(dir.join("projects")) else {
            continue;
        };
        for project in projects.flatten() {
            let path = project.path().join(&file);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Incremental reader for one transcript.
#[derive(Debug)]
pub struct TranscriptTail {
    path: PathBuf,
    /// Bytes consumed so far. Always just past a newline, so a line the
    /// agent is still writing is read whole on the next poll.
    offset: u64,
    pub digest: SessionDigest,
}

impl TranscriptTail {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            digest: SessionDigest::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read what was appended since the last poll. Returns `true` when the
    /// digest changed.
    pub fn poll(&mut self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return false;
        };
        let mut changed = false;
        let len = meta.len();
        if len < self.offset {
            // Rewritten from scratch: start over.
            self.offset = 0;
            self.digest = SessionDigest::default();
            changed = true;
        }
        if len == self.offset {
            return changed;
        }
        let Ok(mut file) = File::open(&self.path) else {
            return changed;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return changed;
        }
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            line.clear();
            let Ok(read) = reader.read_until(b'\n', &mut line) else {
                break;
            };
            if read == 0 || line.last() != Some(&b'\n') {
                break;
            }
            self.offset += read as u64;
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            if let Some(at) = line_timestamp(text)
                && self.digest.last_event_at.is_none_or(|last| at > last)
            {
                self.digest.last_event_at = Some(at);
                changed = true;
            }
            match parse_line(text.trim_end()) {
                Some(Entry::Prompt(prompt)) => {
                    self.digest.push_prompt(prompt);
                    changed = true;
                }
                Some(Entry::Title(title)) if title != self.digest.title => {
                    self.digest.title = title;
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }
}

/// How long a session whose transcript was not found waits before the
/// next lookup. A new session writes its transcript with its first message.
const MISSING_RETRY: Duration = Duration::from_secs(15);

/// Change to one session's digest, sent from the index to the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub enum DigestUpdate {
    Set(String, SessionDigest),
    Remove(String),
}

/// Keeps a [`TranscriptTail`] per tracked Claude session.
#[derive(Debug, Default)]
pub struct TranscriptIndex {
    dirs: Vec<PathBuf>,
    tails: HashMap<String, TranscriptTail>,
    /// Sessions whose transcript was not found, with the time of the lookup.
    missing: HashMap<String, Instant>,
}

impl TranscriptIndex {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            ..Self::default()
        }
    }

    /// Track exactly `sessions` (session id, working directory) and read
    /// what their transcripts gained since the last poll.
    pub fn poll(&mut self, sessions: &[(String, String)]) -> Vec<DigestUpdate> {
        let mut updates = Vec::new();
        let wanted: HashSet<&str> = sessions.iter().map(|(sid, _)| sid.as_str()).collect();
        self.tails.retain(|sid, _| {
            let keep = wanted.contains(sid.as_str());
            if !keep {
                updates.push(DigestUpdate::Remove(sid.clone()));
            }
            keep
        });
        self.missing.retain(|sid, _| wanted.contains(sid.as_str()));

        for (sid, cwd) in sessions {
            if !self.tails.contains_key(sid) {
                if self
                    .missing
                    .get(sid)
                    .is_some_and(|at| at.elapsed() < MISSING_RETRY)
                {
                    continue;
                }
                match find_transcript(sid, cwd, &self.dirs) {
                    Some(path) => {
                        self.missing.remove(sid);
                        self.tails.insert(sid.clone(), TranscriptTail::new(path));
                    }
                    None => {
                        self.missing.insert(sid.clone(), Instant::now());
                        continue;
                    }
                }
            }
            if let Some(tail) = self.tails.get_mut(sid)
                && tail.poll()
            {
                updates.push(DigestUpdate::Set(sid.clone(), tail.digest.clone()));
            }
        }
        updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn user_line(content: &str) -> String {
        serde_json::json!({
            "type": "user",
            "isSidechain": false,
            "message": {"role": "user", "content": content},
            "origin": {"kind": "human"},
        })
        .to_string()
    }

    #[test]
    fn parse_line_reads_typed_prompts() {
        assert_eq!(
            parse_line(&user_line("fix the  login\nbug")),
            Some(Entry::Prompt("fix the login bug".into()))
        );
    }

    #[test]
    fn parse_line_reads_text_blocks() {
        let line = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "look at"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "this"},
            ]},
        })
        .to_string();
        assert_eq!(
            parse_line(&line),
            Some(Entry::Prompt("look at this".into()))
        );
    }

    #[test]
    fn parse_line_skips_tool_results_meta_and_other_origins() {
        let tool_result = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "x", "content": "ok"},
            ]},
        })
        .to_string();
        assert_eq!(parse_line(&tool_result), None);

        let meta = serde_json::json!({
            "type": "user",
            "isMeta": true,
            "message": {"role": "user", "content": "Base directory for this skill"},
        })
        .to_string();
        assert_eq!(parse_line(&meta), None);

        let notification = serde_json::json!({
            "type": "user",
            "origin": {"kind": "task-notification"},
            "message": {"role": "user", "content": "<task-notification>done"},
        })
        .to_string();
        assert_eq!(parse_line(&notification), None);

        let sidechain = serde_json::json!({
            "type": "user",
            "isSidechain": true,
            "message": {"role": "user", "content": "subagent prompt"},
        })
        .to_string();
        assert_eq!(parse_line(&sidechain), None);
    }

    #[test]
    fn parse_line_keeps_typed_and_sdk_turns_and_skips_scheduled_ones() {
        let turn = |turn_origin: &str, prompt_source: &str| {
            serde_json::json!({
                "type": "user",
                "turnOrigin": turn_origin,
                "promptSource": prompt_source,
                "message": {"role": "user", "content": "check the release"},
            })
            .to_string()
        };
        let prompt = Some(Entry::Prompt("check the release".into()));
        assert_eq!(parse_line(&turn("human", "typed")), prompt);
        assert_eq!(parse_line(&turn("human", "queued")), prompt);
        assert_eq!(parse_line(&turn("sdk", "sdk")), prompt);
        assert_eq!(parse_line(&turn("scheduled", "system")), None);
        assert_eq!(parse_line(&turn("peer", "typed")), None);
        assert_eq!(parse_line(&turn("human", "system")), None);
    }

    #[test]
    fn parse_line_skips_injected_text() {
        for text in [
            "<local-command-stdout>Compacted</local-command-stdout>",
            "[Request interrupted by user for tool use]",
            "This session is being continued from a previous conversation that ran out",
            "<system-reminder>x</system-reminder>",
        ] {
            assert_eq!(parse_line(&user_line(text)), None, "{text}");
        }
    }

    #[test]
    fn parse_line_keeps_commands_with_arguments_only() {
        let with_args = "<command-name>/loop</command-name>\n<command-message>loop</command-message>\n<command-args>5m check the deploy</command-args>";
        assert_eq!(
            parse_line(&user_line(with_args)),
            Some(Entry::Prompt("/loop 5m check the deploy".into()))
        );
        let bare = "<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>";
        assert_eq!(parse_line(&user_line(bare)), None);
        let noise = "<command-name>/model</command-name><command-args>opus</command-args>";
        assert_eq!(parse_line(&user_line(noise)), None);
        assert_eq!(parse_line(&user_line("/compact")), None);
        assert_eq!(
            parse_line(&user_line("/review the auth change")),
            Some(Entry::Prompt("/review the auth change".into()))
        );
    }

    #[test]
    fn parse_line_shows_bash_mode_input() {
        assert_eq!(
            parse_line(&user_line("<bash-input>git status</bash-input>")),
            Some(Entry::Prompt("!git status".into()))
        );
    }

    #[test]
    fn parse_line_reads_ai_title() {
        let line = r#"{"type":"ai-title","aiTitle":"Fix login bug","sessionId":"s"}"#;
        assert_eq!(parse_line(line), Some(Entry::Title("Fix login bug".into())));
    }

    #[test]
    fn parse_line_ignores_quoted_type_text_in_assistant_lines() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "the field is \"type\":\"user\""},
            ]},
        })
        .to_string();
        assert_eq!(parse_line(&line), None);
    }

    #[test]
    fn clean_prompt_caps_length() {
        let long = "a".repeat(MAX_PROMPT_CHARS + 50);
        let cleaned = clean_prompt(&long).unwrap();
        assert_eq!(cleaned.chars().count(), MAX_PROMPT_CHARS + 1);
        assert!(cleaned.ends_with('…'));
    }

    #[test]
    fn push_prompt_keeps_goal_when_capped() {
        let mut digest = SessionDigest::default();
        for i in 0..MAX_PROMPTS + 5 {
            digest.push_prompt(format!("p{i}"));
        }
        assert_eq!(digest.prompts.len(), MAX_PROMPTS);
        assert_eq!(digest.goal(), Some("p0"));
        assert_eq!(
            digest.last_prompt(),
            Some(&*format!("p{}", MAX_PROMPTS + 4))
        );
    }

    #[test]
    fn line_timestamp_reads_utc_seconds() {
        let line = r#"{"type":"user","uuid":"u","timestamp":"2026-10-22T06:05:44.370Z"}"#;
        assert_eq!(line_timestamp(line), Some(1_792_649_144));
        assert_eq!(parse_utc_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_utc_timestamp("2000-03-01T00:00:00"),
            Some(951_868_800)
        );
        assert_eq!(line_timestamp(r#"{"type":"bridge-session"}"#), None);
        assert_eq!(parse_utc_timestamp("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn project_slug_replaces_non_alphanumerics() {
        assert_eq!(
            project_slug("/home/u/.config/my_app"),
            "-home-u--config-my-app"
        );
    }

    #[test]
    fn find_transcript_checks_hint_then_scans() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join(".claude");
        let project = config.join("projects").join("-other-place");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("abc-123.jsonl");
        std::fs::write(&path, "").unwrap();

        let dirs = vec![config];
        assert_eq!(
            find_transcript("abc-123", "/somewhere/else", &dirs),
            Some(path)
        );
        assert_eq!(find_transcript("../evil", "", &dirs), None);
        assert_eq!(find_transcript("missing", "", &dirs), None);
    }

    #[test]
    fn tail_reads_appended_lines_and_waits_for_partial_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "{}", user_line("first goal")).unwrap();
        writeln!(file, r#"{{"type":"ai-title","aiTitle":"Title one"}}"#).unwrap();

        let mut tail = TranscriptTail::new(path.clone());
        assert!(tail.poll());
        assert_eq!(tail.digest.goal(), Some("first goal"));
        assert_eq!(tail.digest.title, "Title one");
        assert!(!tail.poll(), "nothing appended, nothing changed");

        // A line still being written is left for the next poll.
        let second = user_line("second ask");
        let (head, rest) = second.split_at(10);
        write!(file, "{head}").unwrap();
        file.flush().unwrap();
        tail.poll();
        assert_eq!(tail.digest.prompts.len(), 1);

        writeln!(file, "{rest}").unwrap();
        file.flush().unwrap();
        assert!(tail.poll());
        assert_eq!(tail.digest.last_prompt(), Some("second ask"));
        assert_eq!(tail.digest.goal(), Some("first goal"));
    }

    #[test]
    fn index_tracks_wanted_sessions_and_reports_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join(".claude");
        let project = config.join("projects").join("-repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("s1.jsonl"),
            format!("{}\n", user_line("goal one")),
        )
        .unwrap();

        let mut index = TranscriptIndex::new(vec![config]);
        let wanted = vec![
            ("s1".to_string(), "/repo".to_string()),
            ("s2".to_string(), "/repo".to_string()),
        ];
        let updates = index.poll(&wanted);
        assert_eq!(updates.len(), 1);
        let DigestUpdate::Set(sid, digest) = &updates[0] else {
            panic!("expected a digest");
        };
        assert_eq!(sid, "s1");
        assert_eq!(digest.goal(), Some("goal one"));

        // Nothing new: no updates. s2 is still missing and not retried yet.
        assert!(index.poll(&wanted).is_empty());

        // Dropping s1 from the wanted set removes it.
        let updates = index.poll(&wanted[1..]);
        assert_eq!(updates, vec![DigestUpdate::Remove("s1".into())]);
    }

    #[test]
    fn tail_starts_over_when_file_shrinks() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.jsonl");
        std::fs::write(&path, format!("{}\n{}\n", user_line("a"), user_line("b"))).unwrap();
        let mut tail = TranscriptTail::new(path.clone());
        tail.poll();
        assert_eq!(tail.digest.prompts.len(), 2);

        std::fs::write(&path, format!("{}\n", user_line("c"))).unwrap();
        assert!(tail.poll());
        assert_eq!(tail.digest.prompts.as_slice(), ["c".to_string()]);
    }
}
