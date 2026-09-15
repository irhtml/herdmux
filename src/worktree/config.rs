use std::path::{Component, Path};

pub const DEFAULT_BRANCH_PREFIX: &str = "agent/";
pub const DEFAULT_WORKTREE_DIR: &str = ".worktrees";
pub const DEFAULT_AGENT: &str = "claude";
pub const DEFAULT_MODE: &str = "default";
/// Untracked files copied from the repo root into a freshly created
/// worktree. `git worktree add` only materialises tracked content, so
/// per-directory environment files (which are deliberately untracked)
/// would otherwise be missing and the agent would launch with the
/// wrong environment — e.g. a `direnv` `.envrc` pinning
/// `CLAUDE_CONFIG_DIR` / `CODEX_HOME` to a work profile.
pub const DEFAULT_COPY_FILES: &str = ".envrc";

pub const AGENT_OPTION: &str = "@agent-sidebar-default-agent";
pub const BRANCH_PREFIX_OPTION: &str = "@agent-sidebar-branch-prefix";
pub const WORKTREE_DIR_OPTION: &str = "@agent-sidebar-worktree-dir";
pub const COPY_FILES_OPTION: &str = "@agent-sidebar-worktree-copy";
pub const DIRENV_ALLOW_OPTION: &str = "@agent-sidebar-worktree-direnv-allow";

pub const AGENTS: &[&str] = &["claude", "codex", "opencode"];
pub const CLAUDE_MODES: &[&str] = &[
    "default",
    "plan",
    "acceptEdits",
    "dontAsk",
    "bypassPermissions",
];
pub const CODEX_MODES: &[&str] = &["default", "auto", "bypassPermissions"];
pub const OPENCODE_MODES: &[&str] = &["default"];

/// Parse the comma-separated `@agent-sidebar-worktree-copy` list into
/// repo-relative paths. `None` (option unset) yields the default list;
/// an explicitly empty value disables copying entirely, which is how a
/// user opts out.
///
/// Entries that are absolute or that walk out of the repo via `..` are
/// dropped rather than rejected: a malformed option should not fail an
/// otherwise valid spawn.
pub fn copy_files_from(raw: Option<&str>) -> Vec<String> {
    let raw = match raw {
        None => DEFAULT_COPY_FILES,
        Some(v) => v,
    };
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter(|entry| {
            let path = Path::new(entry);
            path.is_relative()
                && !path
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::RootDir))
        })
        .map(str::to_string)
        .collect()
}

/// Whether to run `direnv allow` in a freshly created worktree after a
/// `.envrc` was copied into it.
///
/// Off by default, and deliberately so: an `.envrc` is arbitrary shell
/// that `direnv` executes on every `cd`, and approving one on the
/// user's behalf silently widens what runs unattended. Opting in says
/// "the file I am copying is one I already trust in the source repo".
///
/// Accepts `on`/`true`/`1`/`yes` (case-insensitive), matching the
/// `@sidebar_pet` convention.
pub fn direnv_allow_from(raw: Option<&str>) -> bool {
    raw.map(|s| s.trim().to_ascii_lowercase())
        .map(|s| matches!(s.as_str(), "on" | "true" | "1" | "yes"))
        .unwrap_or(false)
}

pub fn modes_for(agent: &str) -> &'static [&'static str] {
    match agent {
        "codex" => CODEX_MODES,
        "opencode" => OPENCODE_MODES,
        _ => CLAUDE_MODES,
    }
}

/// Compose the shell command to run inside the new pane from `agent`
/// and `mode`. Unsupported combinations fall back to launching the
/// agent with no flags.
pub fn agent_command(agent: &str, mode: &str) -> String {
    match (agent, mode) {
        ("claude", "" | "default") => "claude".into(),
        // Only forward modes that Claude Code's `--permission-mode`
        // actually accepts; an unrecognised value (e.g. a stale tmux
        // option from an old build) would otherwise make `claude` exit
        // with an argument error instead of starting up.
        ("claude", m @ ("plan" | "acceptEdits" | "dontAsk" | "bypassPermissions")) => {
            format!("claude --permission-mode {m}")
        }
        ("claude", _) => "claude".into(),
        ("codex", "auto") => "codex --full-auto".into(),
        ("codex", "bypassPermissions") => "codex --dangerously-bypass-approvals-and-sandbox".into(),
        ("codex", _) => "codex".into(),
        ("opencode", _) => "opencode".into(),
        (a, _) => a.to_string(),
    }
}

/// How much to clean up when the user presses `x` on a spawn-created pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveMode {
    /// Close only the owned pane (or legacy dedicated window). Keep git state.
    WindowOnly,
    /// Close the owned pane/window AND remove the worktree and branch.
    WindowAndWorktree,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_files_unset_yields_the_default_list() {
        assert_eq!(copy_files_from(None), vec![DEFAULT_COPY_FILES.to_string()]);
    }

    #[test]
    fn copy_files_explicit_empty_disables_copying() {
        // The documented opt-out: setting the option to an empty string
        // must not silently fall back to the default list.
        assert!(copy_files_from(Some("")).is_empty());
        assert!(copy_files_from(Some("  ,  ")).is_empty());
    }

    #[test]
    fn copy_files_splits_and_trims_entries() {
        assert_eq!(
            copy_files_from(Some(".envrc, .env.local ,tool-versions")),
            vec![
                ".envrc".to_string(),
                ".env.local".to_string(),
                "tool-versions".to_string()
            ]
        );
    }

    #[test]
    fn copy_files_drops_escaping_entries() {
        // A malformed option must not turn a spawn into a way to read
        // files from outside the repo, and must not fail the spawn.
        assert_eq!(
            copy_files_from(Some("/etc/passwd,../../secret,.envrc")),
            vec![".envrc".to_string()]
        );
    }

    #[test]
    fn copy_files_allows_nested_repo_relative_paths() {
        assert_eq!(
            copy_files_from(Some("config/local.toml")),
            vec!["config/local.toml".to_string()]
        );
    }

    #[test]
    fn direnv_allow_defaults_to_off() {
        // Approving an `.envrc` runs arbitrary shell on every `cd`, so
        // an unset or unrecognised option must never enable it.
        assert!(!direnv_allow_from(None));
        assert!(!direnv_allow_from(Some("off")));
        assert!(!direnv_allow_from(Some("")));
        assert!(!direnv_allow_from(Some("maybe")));
    }

    #[test]
    fn direnv_allow_accepts_the_usual_truthy_spellings() {
        for raw in ["on", "ON", " true ", "1", "yes", "Yes"] {
            assert!(direnv_allow_from(Some(raw)), "{raw} should enable");
        }
    }

    #[test]
    fn modes_for_claude_returns_claude_modes_by_default() {
        assert_eq!(modes_for("claude"), CLAUDE_MODES);
    }

    #[test]
    fn modes_for_codex_returns_codex_modes() {
        assert_eq!(modes_for("codex"), CODEX_MODES);
    }

    #[test]
    fn modes_for_opencode_returns_opencode_modes() {
        assert_eq!(modes_for("opencode"), OPENCODE_MODES);
    }

    #[test]
    fn modes_for_unknown_agent_falls_back_to_claude_modes() {
        assert_eq!(modes_for("gemini"), CLAUDE_MODES);
        assert_eq!(modes_for(""), CLAUDE_MODES);
    }

    #[test]
    fn agent_command_claude_variants() {
        assert_eq!(agent_command("claude", ""), "claude");
        assert_eq!(agent_command("claude", "default"), "claude");
        assert_eq!(
            agent_command("claude", "plan"),
            "claude --permission-mode plan"
        );
        assert_eq!(
            agent_command("claude", "acceptEdits"),
            "claude --permission-mode acceptEdits"
        );
    }

    #[test]
    fn agent_command_codex_variants() {
        assert_eq!(agent_command("codex", "default"), "codex");
        assert_eq!(agent_command("codex", ""), "codex");
        assert_eq!(agent_command("codex", "auto"), "codex --full-auto");
        assert_eq!(
            agent_command("codex", "bypassPermissions"),
            "codex --dangerously-bypass-approvals-and-sandbox"
        );
    }

    #[test]
    fn agent_command_opencode_ignores_mode() {
        // OpenCode currently exposes only the default mode; every variant
        // collapses to the bare binary.
        assert_eq!(agent_command("opencode", "default"), "opencode");
        assert_eq!(agent_command("opencode", "plan"), "opencode");
        assert_eq!(agent_command("opencode", ""), "opencode");
    }

    #[test]
    fn agent_command_unknown_agent_passes_through() {
        assert_eq!(agent_command("gemini", "default"), "gemini");
    }

    #[test]
    fn agent_command_claude_unknown_mode_falls_back_to_bare_claude() {
        // Regression: an unknown / stale Claude mode (e.g. persisted
        // from an older build whose mode list has since been removed)
        // used to be forwarded verbatim into `--permission-mode`,
        // making `claude` exit with an argument error on spawn. The
        // documented contract is "fall back to launching the agent
        // with no flags"; this test pins that behaviour.
        assert_eq!(agent_command("claude", "unknown"), "claude");
        assert_eq!(agent_command("claude", "legacy-mode-from-v0"), "claude");
    }

    #[test]
    fn agent_command_claude_all_whitelisted_modes_forwarded() {
        // Every mode the popup offers must still round-trip through
        // `--permission-mode` after the whitelist was introduced.
        for mode in CLAUDE_MODES {
            let got = agent_command("claude", mode);
            let expected = match *mode {
                "default" => "claude".to_string(),
                m => format!("claude --permission-mode {m}"),
            };
            assert_eq!(got, expected, "mode {mode} must match doc contract");
        }
    }

    #[test]
    fn agents_list_matches_modes_for_dispatch() {
        // Every agent listed in the public AGENTS catalog must resolve
        // to a non-empty modes list, otherwise the popup's mode cycle
        // would divide by zero.
        for agent in AGENTS {
            assert!(
                !modes_for(agent).is_empty(),
                "agent {agent} returned an empty mode list"
            );
        }
    }
}
