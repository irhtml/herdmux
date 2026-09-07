use std::path::Path;

use super::config::{
    BRANCH_PREFIX_OPTION, COPY_FILES_OPTION, DIRENV_ALLOW_OPTION, WORKTREE_DIR_OPTION,
};
use crate::{git, tmux};

/// All side effects `spawn_with` / `remove_with` depend on, extracted
/// so failure-path tests can script tmux/git errors without touching a
/// real tmux server or repository. The real implementation lives in
/// `RealEnv` below.
pub(crate) trait SpawnEnv {
    fn branch_prefix(&self) -> Option<String>;
    fn worktree_dir(&self) -> Option<String>;
    /// Raw `@agent-sidebar-worktree-copy` value, or `None` when unset.
    fn copy_files(&self) -> Option<String>;
    /// Copy one repo-relative untracked file into the new worktree.
    /// Missing sources and already-present destinations are successful
    /// no-ops so the caller can stay oblivious to which of the
    /// configured names actually exist in a given repo. `Ok(true)`
    /// means a file was actually written.
    fn copy_into_worktree(&self, repo: &str, worktree: &str, name: &str) -> Result<bool, String>;
    /// Raw `@agent-sidebar-worktree-direnv-allow` value, or `None` when unset.
    fn direnv_allow_option(&self) -> Option<String>;
    /// `direnv allow <worktree>`, so the copied `.envrc` is approved in
    /// its new location instead of being blocked on first `cd`.
    fn direnv_allow(&self, worktree: &str) -> Result<(), String>;
    fn branch_is_free(&self, repo: &str, branch: &str) -> bool;
    /// Whether a branch currently exists in `repo`. Used by the
    /// remove flow to skip `git branch -D` when a previous partial
    /// success already dropped the branch, so retries converge.
    fn branch_exists(&self, repo: &str, branch: &str) -> bool;
    fn worktree_path_is_free(&self, path: &Path) -> bool;
    /// Whether the worktree directory is currently on disk. Distinct
    /// from [`Self::worktree_path_is_free`] only because the fake
    /// implementations in tests need opposite default polarities for
    /// spawn (`is_free=true`, picks a fresh slug) and remove
    /// (`exists=true`, runs the cleanup).
    fn worktree_path_exists(&self, path: &str) -> bool;
    fn worktree_add(&self, repo: &str, worktree_path: &str, branch: &str) -> Result<(), String>;
    fn worktree_remove(&self, repo: &str, worktree_path: &str) -> Result<(), String>;
    fn branch_delete(&self, repo: &str, branch: &str) -> Result<(), String>;
    fn new_window(&self, session: &str, cwd: &str, name: &str) -> Result<(String, String), String>;
    fn kill_window(&self, window_id: &str) -> Result<(), String>;
    fn set_window_option(&self, window: &str, key: &str, value: &str) -> Result<(), String>;
    fn send_command(&self, target: &str, command: &str) -> Result<(), String>;
    fn display_message(&self, pane_id: &str, template: &str) -> String;
}

pub(super) struct RealEnv;

impl SpawnEnv for RealEnv {
    fn branch_prefix(&self) -> Option<String> {
        tmux::get_all_global_options()
            .get(BRANCH_PREFIX_OPTION)
            .cloned()
    }
    fn worktree_dir(&self) -> Option<String> {
        tmux::get_all_global_options()
            .get(WORKTREE_DIR_OPTION)
            .cloned()
    }
    fn copy_files(&self) -> Option<String> {
        tmux::get_all_global_options()
            .get(COPY_FILES_OPTION)
            .cloned()
    }
    fn copy_into_worktree(&self, repo: &str, worktree: &str, name: &str) -> Result<bool, String> {
        let src = Path::new(repo).join(name);
        if !src.is_file() {
            return Ok(false);
        }
        let dst = Path::new(worktree).join(name);
        // `git worktree add` already materialised every tracked file;
        // never clobber one that happens to share a configured name.
        if dst.exists() {
            return Ok(false);
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::copy(&src, &dst)
            .map(|_| true)
            .map_err(|e| format!("{name}: {e}"))
    }
    fn direnv_allow_option(&self) -> Option<String> {
        tmux::get_all_global_options()
            .get(DIRENV_ALLOW_OPTION)
            .cloned()
    }
    fn direnv_allow(&self, worktree: &str) -> Result<(), String> {
        let output = std::process::Command::new("direnv")
            .args(["allow", worktree])
            .output()
            .map_err(|e| format!("failed to spawn direnv: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("direnv exited with status {}", output.status)
        } else {
            stderr
        })
    }
    fn branch_is_free(&self, repo: &str, branch: &str) -> bool {
        !git::branch_exists(repo, branch)
    }
    fn branch_exists(&self, repo: &str, branch: &str) -> bool {
        git::branch_exists(repo, branch)
    }
    fn worktree_path_is_free(&self, path: &Path) -> bool {
        !path.exists()
    }
    fn worktree_path_exists(&self, path: &str) -> bool {
        !path.is_empty() && Path::new(path).exists()
    }
    fn worktree_add(&self, repo: &str, path: &str, branch: &str) -> Result<(), String> {
        git::worktree_add(repo, path, branch)
    }
    fn worktree_remove(&self, repo: &str, path: &str) -> Result<(), String> {
        git::worktree_remove(repo, path)
    }
    fn branch_delete(&self, repo: &str, branch: &str) -> Result<(), String> {
        git::branch_delete(repo, branch)
    }
    fn new_window(&self, session: &str, cwd: &str, name: &str) -> Result<(String, String), String> {
        tmux::new_window(session, cwd, name)
    }
    fn kill_window(&self, window_id: &str) -> Result<(), String> {
        tmux::kill_window(window_id)
    }
    fn set_window_option(&self, window: &str, key: &str, value: &str) -> Result<(), String> {
        tmux::set_window_option(window, key, value)
    }
    fn send_command(&self, target: &str, command: &str) -> Result<(), String> {
        tmux::send_command(target, command)
    }
    fn display_message(&self, pane_id: &str, template: &str) -> String {
        tmux::display_message(pane_id, template)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `copy_into_worktree` is the only `RealEnv` method that touches
    /// the filesystem instead of tmux/git, so its edge cases are worth
    /// pinning against real directories rather than a fake.
    fn repo_and_worktree() -> (tempfile::TempDir, String, String) {
        let root = tempfile::tempdir().expect("tempdir");
        let repo = root.path().join("repo");
        let worktree = root.path().join("repo/.worktrees/task");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        let repo_s = repo.to_str().unwrap().to_string();
        let worktree_s = worktree.to_str().unwrap().to_string();
        (root, repo_s, worktree_s)
    }

    #[test]
    fn copies_an_untracked_env_file_into_the_worktree() {
        let (_root, repo, worktree) = repo_and_worktree();
        fs::write(
            Path::new(&repo).join(".envrc"),
            "export CLAUDE_CONFIG_DIR=\"$HOME/.claude-work\"\n",
        )
        .unwrap();

        assert!(
            RealEnv
                .copy_into_worktree(&repo, &worktree, ".envrc")
                .expect("copy should succeed"),
            "a written file must report true"
        );

        assert_eq!(
            fs::read_to_string(Path::new(&worktree).join(".envrc")).unwrap(),
            "export CLAUDE_CONFIG_DIR=\"$HOME/.claude-work\"\n"
        );
    }

    #[test]
    fn missing_source_is_a_silent_no_op() {
        // Most repos have no `.envrc`; a spawn there must not fail and
        // must not create an empty file in the worktree.
        let (_root, repo, worktree) = repo_and_worktree();
        assert!(
            !RealEnv
                .copy_into_worktree(&repo, &worktree, ".envrc")
                .expect("missing source should be Ok"),
            "a no-op must report false"
        );
        assert!(!Path::new(&worktree).join(".envrc").exists());
    }

    #[test]
    fn existing_destination_is_never_clobbered() {
        // `git worktree add` checks out tracked files first. If a repo
        // tracks a file whose name is also in the copy list, the
        // checked-out content wins.
        let (_root, repo, worktree) = repo_and_worktree();
        fs::write(Path::new(&repo).join(".envrc"), "from repo root\n").unwrap();
        fs::write(Path::new(&worktree).join(".envrc"), "tracked content\n").unwrap();

        assert!(
            !RealEnv
                .copy_into_worktree(&repo, &worktree, ".envrc")
                .expect("existing destination should be Ok"),
            "a skipped copy must report false"
        );

        assert_eq!(
            fs::read_to_string(Path::new(&worktree).join(".envrc")).unwrap(),
            "tracked content\n"
        );
    }

    #[test]
    fn a_directory_named_like_an_entry_is_not_copied() {
        // `.envrc` as a directory is nonsense, but `is_file()` rather
        // than `exists()` is what keeps `fs::copy` from erroring.
        let (_root, repo, worktree) = repo_and_worktree();
        fs::create_dir(Path::new(&repo).join(".envrc")).unwrap();
        assert!(
            !RealEnv
                .copy_into_worktree(&repo, &worktree, ".envrc")
                .expect("directory source should be Ok"),
            "a directory source must report false"
        );
        assert!(!Path::new(&worktree).join(".envrc").exists());
    }

    #[test]
    fn nested_entry_creates_its_parent_directory() {
        let (_root, repo, worktree) = repo_and_worktree();
        fs::create_dir_all(Path::new(&repo).join("config")).unwrap();
        fs::write(Path::new(&repo).join("config/local.toml"), "a = 1\n").unwrap();

        assert!(
            RealEnv
                .copy_into_worktree(&repo, &worktree, "config/local.toml")
                .expect("nested copy should succeed")
        );

        assert_eq!(
            fs::read_to_string(Path::new(&worktree).join("config/local.toml")).unwrap(),
            "a = 1\n"
        );
    }
}
