//! Turn the command line an agent was started with into one that resumes
//! its session: keep the user's flags (`--chrome`, `--model`, …), drop
//! the ones that pick or start a session, and add the session id.

use super::store::Entry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeCommand {
    /// Relaunch into the saved conversation.
    Resume(Vec<String>),
    /// The session never had a turn, so there is nothing to resume:
    /// start the agent again with the same flags.
    Fresh(Vec<String>),
    Skip(String),
}

/// How many values each flag takes. Anything not listed is a switch.
struct Flags {
    value: &'static [&'static str],
    /// Takes a value only when the next token is not a flag.
    optional: &'static [&'static str],
    /// Takes values up to the next flag.
    variadic: &'static [&'static str],
    /// Removed from the resumed command (values included).
    drop: &'static [&'static str],
    /// Their presence means the process was a one-shot run, not a session.
    one_shot: &'static [&'static str],
}

const CLAUDE: Flags = Flags {
    value: &[
        "--agent",
        "--agents",
        "--append-system-prompt",
        "--autocompact",
        "--debug-file",
        "--effort",
        "--environment",
        "--fallback-model",
        "--input-format",
        "--json-schema",
        "--max-budget-usd",
        "--model",
        "-n",
        "--name",
        "--output-format",
        "--permission-mode",
        "--permission-prompts",
        "--plugin-dir",
        "--plugin-url",
        "--remote-control-session-name-prefix",
        "--session-id",
        "--setting-sources",
        "--settings",
        "--system-prompt",
        "--system-prompt-snapshot",
    ],
    optional: &[
        "-r",
        "--resume",
        "-d",
        "--debug",
        "--cloud",
        "--from-pr",
        "--prompt-suggestions",
        "--remote-control",
        "--teleport",
        "-w",
        "--worktree",
    ],
    variadic: &[
        "--add-dir",
        "--allowedTools",
        "--allowed-tools",
        "--betas",
        "--disallowedTools",
        "--disallowed-tools",
        "--file",
        "--mcp-config",
    ],
    drop: &[
        "-r",
        "--resume",
        "-c",
        "--continue",
        "--session-id",
        "--fork-session",
        "-w",
        "--worktree",
        "--tmux",
        "--from-pr",
        "--teleport",
        "--bg",
        "--background",
        "--cloud",
    ],
    one_shot: &["-p", "--print"],
};

const CODEX: Flags = Flags {
    value: &[
        "-c",
        "--config",
        "--enable",
        "--disable",
        "--remote",
        "--remote-auth-token-env",
        "-m",
        "--model",
        "--local-provider",
        "-p",
        "--profile",
        "-s",
        "--sandbox",
        "-C",
        "--cd",
        "--add-dir",
        "-a",
        "--ask-for-approval",
    ],
    optional: &[],
    variadic: &["-i", "--image"],
    // Images belong to the first prompt; `--last` / `--all` are
    // `resume` / `fork` pickers.
    drop: &["-i", "--image", "--last", "--all"],
    one_shot: &[],
};

/// Codex subcommands that are not an interactive session.
const CODEX_ONE_SHOT: &[&str] = &[
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "mcp-server",
    "app-server",
    "completion",
    "sandbox",
    "debug",
    "apply",
    "a",
    "cloud",
    "features",
    "help",
];

const OPENCODE: Flags = Flags {
    value: &[
        "--log-level",
        "-m",
        "--model",
        "-s",
        "--session",
        "-p",
        "--prompt",
        "--agent",
        "--port",
        "--hostname",
    ],
    optional: &[],
    variadic: &[],
    drop: &["-c", "--continue", "-s", "--session", "-p", "--prompt"],
    one_shot: &[],
};

const OPENCODE_SUBCOMMANDS: &[&str] = &[
    "run",
    "serve",
    "auth",
    "agent",
    "upgrade",
    "uninstall",
    "models",
    "stats",
    "export",
    "import",
    "github",
    "mcp",
    "debug",
    "acp",
    "attach",
    "web",
    "pr",
    "session",
    "completion",
    "generate",
];

#[derive(Debug, PartialEq, Eq)]
enum Item {
    Flag { name: String, tokens: Vec<String> },
    Positional(String),
}

fn tokenize(args: &[String], flags: &Flags) -> Vec<Item> {
    let mut items = Vec::new();
    let mut i = 0;
    let is_flag = |s: &str| s.len() > 1 && s.starts_with('-');
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" {
            items.extend(args[i..].iter().cloned().map(Item::Positional));
            break;
        }
        if !is_flag(arg) {
            items.push(Item::Positional(arg.clone()));
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, _)) => (name.to_string(), true),
            None => (arg.clone(), false),
        };
        let mut tokens = vec![arg.clone()];
        if !inline {
            let n = name.as_str();
            if flags.value.contains(&n) {
                if let Some(v) = args.get(i) {
                    tokens.push(v.clone());
                    i += 1;
                }
            } else if flags.optional.contains(&n) {
                if let Some(v) = args.get(i).filter(|v| !is_flag(v)) {
                    tokens.push(v.clone());
                    i += 1;
                }
            } else if flags.variadic.contains(&n) {
                while let Some(v) = args.get(i).filter(|v| !is_flag(v)) {
                    tokens.push(v.clone());
                    i += 1;
                }
            }
        }
        items.push(Item::Flag { name, tokens });
    }
    items
}

/// Flags to keep, or the reason the command is not a resumable session.
fn kept_flags(items: &[Item], flags: &Flags) -> Result<Vec<String>, String> {
    let mut kept = Vec::new();
    for item in items {
        if let Item::Flag { name, tokens } = item {
            if flags.one_shot.contains(&name.as_str()) {
                return Err(format!("{name} runs a one-shot command, not a session"));
            }
            if !flags.drop.contains(&name.as_str()) {
                kept.extend(tokens.iter().cloned());
            }
        }
    }
    Ok(kept)
}

pub(crate) fn build(entry: &Entry) -> ResumeCommand {
    let agent = entry.agent.as_str();
    let Some((first, args)) = entry.argv.split_first() else {
        return ResumeCommand::Skip("no saved command line".into());
    };
    if first != agent {
        return ResumeCommand::Skip(format!("saved command does not start with {agent}"));
    }
    let resume = (entry.had_turn || entry.transcript_found) && !entry.session_id.is_empty();
    let sid = entry.session_id.clone();
    let with_agent = |rest: Vec<String>| std::iter::once(agent.to_string()).chain(rest).collect();

    match agent {
        "claude" => {
            let items = tokenize(args, &CLAUDE);
            match kept_flags(&items, &CLAUDE) {
                Err(reason) => ResumeCommand::Skip(reason),
                Ok(kept) if resume => {
                    ResumeCommand::Resume(with_agent([kept, vec!["--resume".into(), sid]].concat()))
                }
                Ok(kept) => ResumeCommand::Fresh(with_agent(kept)),
            }
        }
        "codex" => {
            let items = tokenize(args, &CODEX);
            let subcommand = items.iter().find_map(|i| match i {
                Item::Positional(p) => Some(p.as_str()),
                Item::Flag { .. } => None,
            });
            if let Some(sub) = subcommand.filter(|s| CODEX_ONE_SHOT.contains(s)) {
                return ResumeCommand::Skip(format!("`codex {sub}` is not an interactive session"));
            }
            match kept_flags(&items, &CODEX) {
                Err(reason) => ResumeCommand::Skip(reason),
                Ok(kept) if resume => ResumeCommand::Resume(with_agent(
                    [vec!["resume".into()], kept, vec![sid]].concat(),
                )),
                Ok(kept) => ResumeCommand::Fresh(with_agent(kept)),
            }
        }
        "opencode" => {
            let items = tokenize(args, &OPENCODE);
            let positional = items.iter().find_map(|i| match i {
                Item::Positional(p) => Some(p.clone()),
                Item::Flag { .. } => None,
            });
            if let Some(sub) = positional
                .as_deref()
                .filter(|s| OPENCODE_SUBCOMMANDS.contains(s))
            {
                return ResumeCommand::Skip(format!(
                    "`opencode {sub}` is not an interactive session"
                ));
            }
            match kept_flags(&items, &OPENCODE) {
                Err(reason) => ResumeCommand::Skip(reason),
                Ok(kept) => {
                    // The optional positional is the project directory.
                    let project = positional.into_iter().collect::<Vec<_>>();
                    let mut argv = with_agent([project, kept].concat());
                    if resume {
                        argv.extend(["--session".into(), sid]);
                        ResumeCommand::Resume(argv)
                    } else {
                        ResumeCommand::Fresh(argv)
                    }
                }
            }
        }
        other => ResumeCommand::Skip(format!("unknown agent `{other}`")),
    }
}

/// Shell line for `argv`, with the saved environment as `VAR=value`
/// assignments in front.
pub(crate) fn render(env: &[(String, String)], argv: &[String]) -> String {
    let quote = crate::cli::setup::shell_quote;
    env.iter()
        .map(|(k, v)| format!("{k}={}", quote(v)))
        .chain(argv.iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(agent: &str, cmd: &str) -> Entry {
        Entry {
            agent: agent.into(),
            session_id: "SID".into(),
            argv: cmd.split(' ').map(str::to_string).collect(),
            had_turn: true,
            ..Default::default()
        }
    }

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn claude_keeps_user_flags_and_appends_resume() {
        assert_eq!(
            build(&entry("claude", "claude --chrome")),
            ResumeCommand::Resume(argv("claude --chrome --resume SID"))
        );
        assert_eq!(
            build(&entry(
                "claude",
                "claude --model opus --add-dir /a /b --permission-mode plan --dangerously-skip-permissions"
            )),
            ResumeCommand::Resume(argv(
                "claude --model opus --add-dir /a /b --permission-mode plan --dangerously-skip-permissions --resume SID"
            ))
        );
    }

    #[test]
    fn claude_drops_session_pickers_and_the_initial_prompt() {
        for cmd in [
            "claude --chrome --resume",
            "claude --chrome -r OLD",
            "claude --chrome --resume=OLD",
            "claude -c --chrome",
            "claude --chrome --session-id OLD fix-the-bug",
            "claude --chrome --fork-session -r OLD",
            "claude --chrome -w",
        ] {
            assert_eq!(
                build(&entry("claude", cmd)),
                ResumeCommand::Resume(argv("claude --chrome --resume SID")),
                "{cmd}"
            );
        }
    }

    #[test]
    fn claude_print_mode_is_skipped() {
        assert!(matches!(
            build(&entry("claude", "claude -p hello")),
            ResumeCommand::Skip(_)
        ));
    }

    #[test]
    fn never_prompted_session_restarts_fresh() {
        let fresh = Entry {
            had_turn: false,
            transcript_found: false,
            ..entry("claude", "claude --chrome")
        };
        assert_eq!(build(&fresh), ResumeCommand::Fresh(argv("claude --chrome")));
        let transcript_only = Entry {
            had_turn: false,
            transcript_found: true,
            ..entry("claude", "claude")
        };
        assert_eq!(
            build(&transcript_only),
            ResumeCommand::Resume(argv("claude --resume SID"))
        );
        let no_id = Entry {
            session_id: String::new(),
            ..entry("claude", "claude")
        };
        assert_eq!(build(&no_id), ResumeCommand::Fresh(argv("claude")));
    }

    #[test]
    fn codex_resumes_through_the_subcommand() {
        assert_eq!(
            build(&entry("codex", "codex --yolo -m o3")),
            ResumeCommand::Resume(argv("codex resume --yolo -m o3 SID"))
        );
        assert_eq!(
            build(&entry("codex", "codex resume OLD --full-auto")),
            ResumeCommand::Resume(argv("codex resume --full-auto SID"))
        );
        assert_eq!(
            build(&entry("codex", "codex resume --last -c model=o3")),
            ResumeCommand::Resume(argv("codex resume -c model=o3 SID"))
        );
        assert_eq!(
            build(&entry("codex", "codex -i a.png b.png fix-it")),
            ResumeCommand::Resume(argv("codex resume SID"))
        );
        assert!(matches!(
            build(&entry("codex", "codex exec --full-auto fix")),
            ResumeCommand::Skip(_)
        ));
    }

    #[test]
    fn opencode_keeps_the_project_and_swaps_the_session() {
        assert_eq!(
            build(&entry("opencode", "opencode /repo -m gpt -s OLD")),
            ResumeCommand::Resume(argv("opencode /repo -m gpt --session SID"))
        );
        assert!(matches!(
            build(&entry("opencode", "opencode run hi")),
            ResumeCommand::Skip(_)
        ));
    }

    #[test]
    fn mismatched_or_empty_argv_is_skipped() {
        assert!(matches!(
            build(&entry("claude", "node cli.js")),
            ResumeCommand::Skip(_)
        ));
        let empty = Entry {
            argv: vec![],
            ..entry("claude", "claude")
        };
        assert!(matches!(build(&empty), ResumeCommand::Skip(_)));
    }

    #[test]
    fn render_quotes_values_and_prefixes_env() {
        let env = vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            "/home/u/.claude fr24".to_string(),
        )];
        let line = render(
            &env,
            &[
                "claude".into(),
                "--append-system-prompt".into(),
                "be brief; no $HOME".into(),
            ],
        );
        assert_eq!(
            line,
            "CLAUDE_CONFIG_DIR='/home/u/.claude fr24' claude --append-system-prompt 'be brief; no $HOME'"
        );
    }
}
