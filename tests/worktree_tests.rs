use std::path::PathBuf;

use herdmux::worktree::{
    AGENTS, CLAUDE_MODES, CODEX_MODES, agent_command, modes_for, pick_unique_slug, slugify,
    worktree_path_for,
};

#[test]
fn slugify_lowercases_and_hyphenates_spaces() {
    assert_eq!(slugify("Add login form"), "add-login-form");
}

#[test]
fn slugify_collapses_runs_of_separators() {
    assert_eq!(slugify("Fix --  the   bug!!"), "fix-the-bug");
}

#[test]
fn slugify_strips_leading_and_trailing_punctuation() {
    assert_eq!(slugify("--hello--"), "hello");
    assert_eq!(slugify("   .. world .. "), "world");
}

#[test]
fn slugify_keeps_digits() {
    assert_eq!(slugify("issue 123 fix"), "issue-123-fix");
}

#[test]
fn slugify_returns_empty_for_pure_punctuation() {
    assert_eq!(slugify("!!!"), "");
    assert_eq!(slugify("   "), "");
    assert_eq!(slugify(""), "");
}

#[test]
fn slugify_drops_unicode_and_symbols() {
    // Non-ASCII characters are dropped; surrounding ASCII is still joined.
    assert_eq!(slugify("日本語 task"), "task");
}

#[test]
fn pick_unique_slug_returns_input_when_free() {
    let picked = pick_unique_slug("foo", |_| true).unwrap();
    assert_eq!(picked, "foo");
}

#[test]
fn pick_unique_slug_appends_suffix_on_collision() {
    let taken = ["foo".to_string()];
    let picked = pick_unique_slug("foo", |s| !taken.contains(&s.to_string())).unwrap();
    assert_eq!(picked, "foo-2");
}

#[test]
fn pick_unique_slug_skips_multiple_taken_suffixes() {
    let taken: Vec<String> = ["foo", "foo-2", "foo-3", "foo-4"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let picked = pick_unique_slug("foo", |s| !taken.contains(&s.to_string())).unwrap();
    assert_eq!(picked, "foo-5");
}

#[test]
fn pick_unique_slug_returns_none_when_exhausted() {
    let picked = pick_unique_slug("foo", |_| false);
    assert!(picked.is_none());
}

#[test]
fn worktree_path_uses_default_repo_local_directory() {
    let repo = PathBuf::from("/home/jess/code/myproj");
    let path = worktree_path_for(&repo, "feature", None).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/jess/code/myproj/.worktrees/feature")
    );
}

#[test]
fn worktree_path_uses_custom_repo_relative_directory() {
    let repo = PathBuf::from("/home/jess/code/myproj");
    let path = worktree_path_for(&repo, "feature", Some(".worktrees")).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/jess/code/myproj/.worktrees/feature")
    );
}

#[test]
fn worktree_path_handles_nested_custom_directory() {
    let repo = PathBuf::from("/home/jess/code/myproj");
    let path = worktree_path_for(&repo, "task-2", Some("tmp/worktrees")).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/jess/code/myproj/tmp/worktrees/task-2")
    );
}

#[test]
fn worktree_path_empty_custom_directory_falls_back_to_default() {
    let repo = PathBuf::from("/tmp/repo");
    let path = worktree_path_for(&repo, "task-2", Some("")).unwrap();
    assert_eq!(path, PathBuf::from("/tmp/repo/.worktrees/task-2"));
}

#[test]
fn worktree_path_rejects_absolute_custom_directory() {
    let repo = PathBuf::from("/tmp/repo");
    assert!(worktree_path_for(&repo, "task-2", Some("/tmp/worktrees")).is_none());
}

#[test]
fn worktree_path_rejects_parent_relative_custom_directory() {
    let repo = PathBuf::from("/tmp/repo");
    assert!(worktree_path_for(&repo, "task-2", Some("../worktrees")).is_none());
}

// ─── agent_command / modes_for ───────────────────────────────────────────

#[test]
fn agent_command_claude_default_has_no_flag() {
    assert_eq!(agent_command("claude", "default"), "claude");
    assert_eq!(agent_command("claude", ""), "claude");
}

#[test]
fn agent_command_claude_nondefault_uses_permission_mode_flag() {
    assert_eq!(
        agent_command("claude", "plan"),
        "claude --permission-mode plan"
    );
    assert_eq!(
        agent_command("claude", "acceptEdits"),
        "claude --permission-mode acceptEdits"
    );
    assert_eq!(
        agent_command("claude", "dontAsk"),
        "claude --permission-mode dontAsk"
    );
    assert_eq!(
        agent_command("claude", "bypassPermissions"),
        "claude --permission-mode bypassPermissions"
    );
}

#[test]
fn agent_command_codex_maps_to_known_flags() {
    assert_eq!(agent_command("codex", "default"), "codex");
    assert_eq!(agent_command("codex", "auto"), "codex --full-auto");
    assert_eq!(
        agent_command("codex", "bypassPermissions"),
        "codex --dangerously-bypass-approvals-and-sandbox"
    );
}

#[test]
fn agent_command_codex_unknown_mode_falls_back_to_bare_codex() {
    assert_eq!(agent_command("codex", "plan"), "codex");
    assert_eq!(agent_command("codex", ""), "codex");
}

#[test]
fn agent_command_unknown_agent_is_echoed_raw() {
    assert_eq!(agent_command("opencode", "default"), "opencode");
}

#[test]
fn modes_for_claude_returns_claude_modes() {
    assert_eq!(modes_for("claude"), CLAUDE_MODES);
}

#[test]
fn modes_for_codex_returns_codex_modes() {
    assert_eq!(modes_for("codex"), CODEX_MODES);
}

#[test]
fn modes_for_unknown_agent_defaults_to_claude_list() {
    assert_eq!(modes_for("gemini"), CLAUDE_MODES);
    assert_eq!(modes_for(""), CLAUDE_MODES);
}

#[test]
fn agents_list_is_non_empty_and_unique() {
    assert!(!AGENTS.is_empty());
    let mut seen = std::collections::HashSet::new();
    for a in AGENTS {
        assert!(seen.insert(*a), "duplicate agent {a:?}");
    }
}

/// Exercise the real CLI and tmux commands on a private server, never the user's server.
#[test]
fn spawn_cli_splits_originating_window_and_keeps_sidebar_and_siblings_unmarked() {
    use std::path::Path;
    use std::process::Command;

    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping isolated tmux integration test: tmux is not installed");
        return;
    }
    struct Server(PathBuf);
    impl Server {
        fn run(&self, args: &[&str]) -> String {
            let output = Command::new("tmux")
                .arg("-S")
                .arg(&self.0)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "tmux {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&self.0)
                .arg("kill-server")
                .output();
        }
    }
    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "fixture",
        ],
    );
    let server = Server(temp.path().join("tmux.sock"));
    let repo_str = repo.to_str().unwrap();
    let main = server.run(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "fixture",
        "-x",
        "140",
        "-y",
        "40",
        "-c",
        repo_str,
        "-P",
        "-F",
        "#{pane_id}",
        "sleep 60",
    ]);
    let window = server.run(&["display-message", "-p", "-t", &main, "#{window_id}"]);
    let sidebar = server.run(&[
        "split-window",
        "-h",
        "-l",
        "28",
        "-t",
        &main,
        "-c",
        repo_str,
        "-P",
        "-F",
        "#{pane_id}",
        "sleep 60",
    ]);
    server.run(&["set", "-p", "-t", &sidebar, "@pane_role", "sidebar"]);
    server.run(&["set", "-g", "@agent-sidebar-default-agent", "true"]);
    let sidebar_width = server.run(&["display-message", "-p", "-t", &sidebar, "#{pane_width}"]);
    // Make another window current: the spawn must still target the origin, not tmux's default.
    server.run(&["new-window", "-t", "fixture", "sleep 60"]);

    for (origin, task) in [(&sidebar, "sidebar-task"), (&main, "cli-task")] {
        let output = Command::new(env!("CARGO_BIN_EXE_herdmux"))
            .args(["spawn", task])
            .env("TMUX", format!("{},0,0", server.0.display()))
            .env("TMUX_PANE", origin)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "spawn: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(repo.join(".worktrees").join(task).is_dir());
    }
    assert_eq!(
        server
            .run(&["list-windows", "-t", "fixture", "-F", "#{window_id}"])
            .lines()
            .count(),
        2
    );
    assert_eq!(
        server
            .run(&["list-panes", "-t", &window, "-F", "#{pane_id}"])
            .lines()
            .count(),
        4
    );
    assert_eq!(
        server.run(&["display-message", "-p", "-t", &sidebar, "#{pane_width}"]),
        sidebar_width
    );
    for sibling in [&main, &sidebar] {
        assert_eq!(
            server.run(&[
                "display-message",
                "-p",
                "-t",
                sibling,
                "#{@agent-sidebar-spawned}"
            ]),
            ""
        );
    }
    assert_eq!(
        server.run(&[
            "show-options",
            "-w",
            "-t",
            &window,
            "-qv",
            "@agent-sidebar-spawned"
        ]),
        ""
    );
    let markers = server.run(&[
        "list-panes",
        "-t",
        &window,
        "-F",
        "#{@agent-sidebar-spawned}:#{@agent-sidebar-spawned-scope}",
    ]);
    assert_eq!(markers.lines().filter(|line| *line == "1:pane").count(), 2);
}

#[test]
fn mode_lists_start_with_default() {
    assert_eq!(CLAUDE_MODES.first().copied(), Some("default"));
    assert_eq!(CODEX_MODES.first().copied(), Some("default"));
}
