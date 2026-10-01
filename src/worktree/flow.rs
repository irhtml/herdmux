use std::path::{Component, Path, PathBuf};

use super::config::{DEFAULT_BRANCH_PREFIX, RemoveMode, copy_files_from, direnv_allow_from};
use super::env::{RealEnv, SpawnEnv};
use super::markers::{
    SPAWNED_BRANCH_OPTION, SPAWNED_FROM_OPTION, SPAWNED_OPTION, SPAWNED_SCOPE_OPTION,
    SPAWNED_WORKTREE_OPTION, SpawnMarkers, spawn_markers_template,
};
use super::slug::{MAX_COLLISION_ATTEMPTS, pick_unique_slug, slugify, worktree_path_for};

#[derive(Debug, Clone, Default)]
pub struct SpawnRequest {
    pub repo_root: PathBuf,
    pub task_name: String,
    /// Originating pane: pins the window even if the user switches tabs while spawning.
    pub origin_pane: String,
    pub agent: String,
    pub mode: String,
    /// Keep focus on the originating pane instead of the new split.
    pub detached: bool,
    /// Extra agent CLI arguments, shell-quoted onto the launch command.
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnOutcome {
    pub branch: String,
    pub pane_id: String,
    pub worktree_path: String,
}

/// Create a worktree, split the originating tmux window, launch the agent,
/// and stash markers at pane scope so the matching `x` flow can find
/// it later. Returns the resulting branch name on success. On any
/// failure past `git worktree add` the worktree is rolled back.
pub fn spawn(req: &SpawnRequest) -> Result<String, String> {
    spawn_detailed(req).map(|outcome| outcome.branch)
}

/// [`spawn`], but also reports the new pane and worktree path so a
/// caller can keep driving the agent it launched.
pub fn spawn_detailed(req: &SpawnRequest) -> Result<SpawnOutcome, String> {
    spawn_with(&RealEnv, req)
}

/// Shell command line that launches `agent` in `mode`, followed by
/// `extra_args` quoted so the shell passes each one through verbatim.
pub fn launch_command(agent: &str, mode: &str, extra_args: &[String]) -> String {
    std::iter::once(super::config::agent_command(agent, mode))
        .chain(extra_args.iter().map(|a| crate::cli::setup::shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn spawn_with<E: SpawnEnv>(env: &E, req: &SpawnRequest) -> Result<SpawnOutcome, String> {
    let slug = slugify(&req.task_name);
    if slug.is_empty() {
        return Err("name is empty after slugification".into());
    }
    let repo = req
        .repo_root
        .to_str()
        .ok_or("repo root is not valid UTF-8")?;
    // Resolve before checkout so missing panes don't create orphaned worktrees.
    let target = env
        .split_target(&req.origin_pane)
        .map_err(|e| format!("tmux: {e}"))?;

    let prefix = env
        .branch_prefix()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BRANCH_PREFIX.to_string());
    let worktree_dir = env.worktree_dir();
    if worktree_dir.as_deref().is_some_and(|dir| {
        let dir = Path::new(dir);
        !dir.as_os_str().is_empty()
            && (dir.is_absolute() || dir.components().any(|c| c == Component::ParentDir))
    }) {
        return Err("worktree directory must be repo-relative".into());
    }

    let unique = pick_unique_slug(&slug, |s| {
        let branch = format!("{prefix}{s}");
        env.branch_is_free(repo, &branch)
            && worktree_path_for(&req.repo_root, s, worktree_dir.as_deref())
                .is_some_and(|p| env.worktree_path_is_free(&p))
    })
    .ok_or_else(|| format!("no free branch name found after {MAX_COLLISION_ATTEMPTS} attempts"))?;

    let branch = format!("{prefix}{unique}");
    let worktree_path = worktree_path_for(&req.repo_root, &unique, worktree_dir.as_deref())
        .ok_or("worktree directory must be repo-relative")?;
    let worktree = worktree_path.to_str().ok_or("worktree path is not UTF-8")?;

    env.worktree_add(repo, worktree, &branch)
        .map_err(|e| format!("git: {e}"))?;

    // `git worktree add` only checks out tracked content, so untracked
    // per-directory environment files (`.envrc` and friends) do not
    // follow. Copy them before the agent launches, otherwise the new
    // pane starts with the wrong profile — e.g. a `direnv` `.envrc`
    // pinning CLAUDE_CONFIG_DIR to a work config dir would be missing
    // and the agent would silently fall back to the personal one.
    //
    // Best effort on purpose: the worktree and branch already exist at
    // this point, and a copy failure is cosmetic next to rolling back a
    // successful checkout.
    let mut copied_envrc = false;
    for name in copy_files_from(env.copy_files().as_deref()) {
        if env.copy_into_worktree(repo, worktree, &name) == Ok(true) && name == ".envrc" {
            copied_envrc = true;
        }
    }

    // A copied `.envrc` is unapproved in its new path, so direnv blocks
    // it and the pane starts without the exported variables. Approving
    // it runs arbitrary shell on every `cd`, so this stays opt-in and
    // only fires for an `.envrc` this spawn actually wrote.
    if copied_envrc && direnv_allow_from(env.direnv_allow_option().as_deref()) {
        let _ = env.direnv_allow(worktree);
    }

    let pane_id = env
        .split_pane(&target, worktree, req.detached)
        .map_err(|e| {
            let rb = rollback_spawn(env, repo, worktree, &branch, None);
            compose_spawn_error(format!("tmux: {e}"), rb)
        })?;

    // Pane scope: sibling panes in the shared window must not inherit ownership.
    for (key, value) in [
        // Mask inherited legacy ownership while writing local markers.
        // Scope precedes the remaining markers; ownership is enabled last.
        (SPAWNED_OPTION, "0"),
        (SPAWNED_SCOPE_OPTION, "pane"),
        (SPAWNED_FROM_OPTION, repo),
        (SPAWNED_WORKTREE_OPTION, worktree),
        (SPAWNED_BRANCH_OPTION, &branch),
        (SPAWNED_OPTION, "1"),
    ] {
        if let Err(e) = env.set_pane_option(&pane_id, key, value) {
            let rb = rollback_spawn(env, repo, worktree, &branch, Some(&pane_id));
            return Err(compose_spawn_error(
                format!("tmux: failed to set {key}: {e}"),
                rb,
            ));
        }
    }

    if let Err(e) = env.send_command(
        &pane_id,
        &launch_command(&req.agent, &req.mode, &req.extra_args),
    ) {
        let rb = rollback_spawn(env, repo, worktree, &branch, Some(&pane_id));
        return Err(compose_spawn_error(format!("tmux: {e}"), rb));
    }

    Ok(SpawnOutcome {
        branch,
        pane_id,
        worktree_path: worktree.to_string(),
    })
}

/// Best-effort rollback after a partial spawn. Kills only the created pane
/// (when one was created), removes the git worktree, and deletes the
/// branch ref that `git worktree add -b` created. Each step collects
/// its error so the caller can surface a full picture of what is
/// still lying around on disk / in tmux. Deleting the branch is
/// important: `worktree remove --force` leaves the branch behind,
/// which later spawns would then collide with.
fn rollback_spawn<E: SpawnEnv>(
    env: &E,
    repo: &str,
    worktree_path: &str,
    branch: &str,
    pane_id: Option<&str>,
) -> Vec<String> {
    let mut errs = Vec::new();
    if let Some(pane_id) = pane_id
        && let Err(e) = env.kill_pane(pane_id)
    {
        // Keep the checkout if an agent may still be running in it.
        errs.push(format!("kill_pane: {e}"));
        return errs;
    }
    if let Err(e) = env.worktree_remove(repo, worktree_path) {
        errs.push(format!("worktree_remove: {e}"));
    }
    if let Err(e) = env.branch_delete(repo, branch) {
        errs.push(format!("branch_delete: {e}"));
    }
    errs
}

/// Combine the primary spawn error with any rollback failures so the
/// user sees a single string that covers both the trigger and the
/// state left behind.
fn compose_spawn_error(primary: String, rollback_errs: Vec<String>) -> String {
    if rollback_errs.is_empty() {
        primary
    } else {
        format!(
            "{primary} (rollback incomplete: {})",
            rollback_errs.join("; ")
        )
    }
}

/// Tear down a previously-spawned pane. Runs ALL git cleanup
/// (`worktree remove --force`, then `git branch -D`) BEFORE killing
/// the owned pane (or legacy window) so a git failure leaves the UI
/// handle and its markers intact. This is the only handle the
/// retry path depends on, so killing it first would strand any
/// leftover git state with no way to finish cleanup from the
/// sidebar. Each git step is skipped when its target is already
/// gone (`worktree_path_exists` / `branch_exists`), which lets
/// retries after a partial-success failure converge.
pub fn remove(pane_id: &str, mode: RemoveMode) -> Result<(), String> {
    remove_with(&RealEnv, pane_id, mode)
}

pub(crate) fn remove_with<E: SpawnEnv>(
    env: &E,
    pane_id: &str,
    mode: RemoveMode,
) -> Result<(), String> {
    let markers = SpawnMarkers::parse(&env.display_message(pane_id, &spawn_markers_template()));
    if !markers.is_spawned() {
        return Err("pane was not created by sidebar spawn".into());
    }
    if markers.worktree_path.is_empty() {
        return Err("spawned worktree path is unset".into());
    }
    if markers.branch.is_empty() {
        return Err("spawned branch is unset".into());
    }
    if markers.window_id.is_empty() {
        return Err("could not resolve window id".into());
    }

    if mode == RemoveMode::WindowAndWorktree {
        if env.worktree_path_exists(&markers.worktree_path) {
            env.worktree_remove(&markers.from_repo, &markers.worktree_path)
                .map_err(|e| format!("git: {e}"))?;
        }
        // `git worktree remove` leaves the branch ref behind; drop
        // it here, before `kill_window`, so a failure still leaves
        // the window as a retry handle.
        if env.branch_exists(&markers.from_repo, &markers.branch) {
            env.branch_delete(&markers.from_repo, &markers.branch)
                .map_err(|e| format!("git: {e}"))?;
        }
    }
    if markers.pane_scoped {
        env.kill_pane(pane_id)
    } else {
        // Compatibility with worktrees spawned into dedicated windows by older versions.
        env.kill_window(&markers.window_id)
    }
    .map_err(|e| format!("tmux: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod env_tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Default)]
    struct FakeEnv {
        calls: RefCell<Vec<String>>,
        set_option_calls: RefCell<usize>,
        worktree_dir: Option<String>,
        fail_set_option_at: Option<usize>,
        fail_kill_window: bool,
        fail_kill_pane: bool,
        fail_worktree_remove: bool,
        fail_branch_delete: bool,
        fail_send_command: bool,
        display_output: Option<String>,
        /// Programs `worktree_path_is_free`. `None` = default false
        /// (the worktree exists on disk). `Some(true)` = the path was
        /// already cleaned up (e.g. on a retry after a partial
        /// failure) so the remove flow should skip the git step.
        worktree_path_already_gone: Option<bool>,
        /// Programs `branch_exists` for remove tests. `None` / `false`
        /// = branch still present (default), `Some(true)` = the
        /// branch was already dropped by a previous partial success
        /// so the remove flow should skip `git branch -D`.
        branch_already_gone: Option<bool>,
        /// Raw `@agent-sidebar-worktree-copy` override; `None` exercises
        /// the built-in default list.
        copy_files: Option<String>,
        /// Raw `@agent-sidebar-worktree-direnv-allow` override.
        direnv_allow: Option<String>,
        /// Make every copy report "nothing written", as happens when the
        /// repo has no such file.
        copy_is_noop: bool,
    }

    impl FakeEnv {
        fn log(&self, s: String) {
            self.calls.borrow_mut().push(s);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl SpawnEnv for FakeEnv {
        fn branch_prefix(&self) -> Option<String> {
            None
        }
        fn worktree_dir(&self) -> Option<String> {
            self.worktree_dir.clone()
        }
        fn copy_files(&self) -> Option<String> {
            self.copy_files.clone()
        }
        fn copy_into_worktree(
            &self,
            repo: &str,
            worktree: &str,
            name: &str,
        ) -> Result<bool, String> {
            self.log(format!("copy_into_worktree({repo},{worktree},{name})"));
            Ok(!self.copy_is_noop)
        }
        fn direnv_allow_option(&self) -> Option<String> {
            self.direnv_allow.clone()
        }
        fn direnv_allow(&self, worktree: &str) -> Result<(), String> {
            self.log(format!("direnv_allow({worktree})"));
            Ok(())
        }
        fn branch_is_free(&self, _repo: &str, _branch: &str) -> bool {
            true
        }
        fn branch_exists(&self, _repo: &str, _branch: &str) -> bool {
            !self.branch_already_gone.unwrap_or(false)
        }
        fn worktree_path_is_free(&self, _path: &Path) -> bool {
            true
        }
        fn worktree_path_exists(&self, _path: &str) -> bool {
            !self.worktree_path_already_gone.unwrap_or(false)
        }
        fn worktree_add(&self, repo: &str, path: &str, branch: &str) -> Result<(), String> {
            self.log(format!("worktree_add({repo},{path},{branch})"));
            Ok(())
        }
        fn worktree_remove(&self, repo: &str, path: &str) -> Result<(), String> {
            self.log(format!("worktree_remove({repo},{path})"));
            if self.fail_worktree_remove {
                Err("worktree_remove failed".into())
            } else {
                Ok(())
            }
        }
        fn branch_delete(&self, repo: &str, branch: &str) -> Result<(), String> {
            self.log(format!("branch_delete({repo},{branch})"));
            if self.fail_branch_delete {
                Err("branch_delete failed".into())
            } else {
                Ok(())
            }
        }
        fn split_target(&self, origin: &str) -> Result<String, String> {
            self.log(format!("split_target({origin})"));
            Ok("%0".into())
        }
        fn split_pane(&self, target: &str, cwd: &str, detached: bool) -> Result<String, String> {
            let suffix = if detached { ",detached" } else { "" };
            self.log(format!("split_pane({target},{cwd}{suffix})"));
            Ok("%1".into())
        }
        fn kill_pane(&self, pane: &str) -> Result<(), String> {
            self.log(format!("kill_pane({pane})"));
            if self.fail_kill_pane {
                Err("kill_pane failed".into())
            } else {
                Ok(())
            }
        }
        fn kill_window(&self, window_id: &str) -> Result<(), String> {
            self.log(format!("kill_window({window_id})"));
            if self.fail_kill_window {
                Err("kill_window failed".into())
            } else {
                Ok(())
            }
        }
        fn set_pane_option(&self, pane: &str, key: &str, value: &str) -> Result<(), String> {
            let idx = *self.set_option_calls.borrow();
            *self.set_option_calls.borrow_mut() += 1;
            self.log(format!("set_pane_option({pane},{key},{value})"));
            if Some(idx) == self.fail_set_option_at {
                Err(format!("set {key} failed"))
            } else {
                Ok(())
            }
        }
        fn send_command(&self, target: &str, command: &str) -> Result<(), String> {
            self.log(format!("send_command({target},{command})"));
            if self.fail_send_command {
                Err("send_command failed".into())
            } else {
                Ok(())
            }
        }
        fn display_message(&self, _pane_id: &str, _template: &str) -> String {
            self.display_output
                .clone()
                .unwrap_or_else(|| "1\n/r\n/r/.worktrees/task\nagent/task\n@1\n".into())
        }
    }

    fn sample_req() -> SpawnRequest {
        SpawnRequest {
            repo_root: PathBuf::from("/r"),
            task_name: "task".into(),
            origin_pane: "%0".into(),
            agent: "claude".into(),
            mode: "default".into(),
            ..Default::default()
        }
    }

    fn has_call(calls: &[String], prefix: &str) -> bool {
        calls.iter().any(|c| c.starts_with(prefix))
    }

    #[test]
    fn spawn_happy_path_sets_all_markers_then_sends_command() {
        let env = FakeEnv::default();
        let outcome = spawn_with(&env, &sample_req()).expect("spawn should succeed");
        assert_eq!(outcome.branch, "agent/task");
        assert_eq!(outcome.pane_id, "%1");
        assert_eq!(outcome.worktree_path, "/r/.worktrees/task");
        let calls = env.calls();
        assert!(has_call(
            &calls,
            "worktree_add(/r,/r/.worktrees/task,agent/task)"
        ));
        assert!(has_call(&calls, "split_pane(%0,/r/.worktrees/task)"));
        assert_eq!(*env.set_option_calls.borrow(), 6);
        assert!(has_call(
            &calls,
            "set_pane_option(%1,@agent-sidebar-spawned-scope,pane)"
        ));
        assert!(has_call(&calls, "send_command(%1,claude"));
        assert!(
            !has_call(&calls, "kill_window("),
            "no rollback on happy path"
        );
    }

    #[test]
    fn detached_spawn_appends_quoted_extra_args() {
        let env = FakeEnv::default();
        let req = SpawnRequest {
            detached: true,
            extra_args: vec!["--model".into(), "opus".into(), "two words".into()],
            ..sample_req()
        };
        spawn_with(&env, &req).expect("spawn should succeed");
        let calls = env.calls();
        assert!(has_call(
            &calls,
            "split_pane(%0,/r/.worktrees/task,detached)"
        ));
        assert!(has_call(
            &calls,
            "send_command(%1,claude --model opus 'two words')"
        ));
    }

    #[test]
    fn launch_command_without_extra_args_matches_agent_command() {
        assert_eq!(
            launch_command("codex", "auto", &[]),
            super::super::config::agent_command("codex", "auto")
        );
    }

    #[test]
    fn spawn_copies_envrc_into_the_new_worktree_before_launching() {
        let env = FakeEnv::default();
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        let calls = env.calls();
        assert!(has_call(
            &calls,
            "copy_into_worktree(/r,/r/.worktrees/task,.envrc)"
        ));
        // Ordering matters: the file has to land before the agent
        // starts, otherwise direnv has nothing to load for the pane.
        let copy = calls
            .iter()
            .position(|c| c.starts_with("copy_into_worktree("))
            .expect("copy call recorded");
        let add = calls
            .iter()
            .position(|c| c.starts_with("worktree_add("))
            .expect("worktree_add call recorded");
        let window = calls
            .iter()
            .position(|c| c.starts_with("split_pane("))
            .expect("split_pane call recorded");
        assert!(add < copy && copy < window, "calls: {calls:?}");
    }

    #[test]
    fn spawn_copies_every_configured_file() {
        let env = FakeEnv {
            copy_files: Some(".envrc,.env.local".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        let calls = env.calls();
        assert!(has_call(
            &calls,
            "copy_into_worktree(/r,/r/.worktrees/task,.envrc)"
        ));
        assert!(has_call(
            &calls,
            "copy_into_worktree(/r,/r/.worktrees/task,.env.local)"
        ));
    }

    #[test]
    fn spawn_skips_copying_when_the_option_is_empty() {
        let env = FakeEnv {
            copy_files: Some("".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        assert!(!has_call(&env.calls(), "copy_into_worktree("));
    }

    #[test]
    fn spawn_does_not_run_direnv_allow_by_default() {
        let env = FakeEnv::default();
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        assert!(
            !has_call(&env.calls(), "direnv_allow("),
            "direnv approval must stay opt-in"
        );
    }

    #[test]
    fn spawn_runs_direnv_allow_when_opted_in() {
        let env = FakeEnv {
            direnv_allow: Some("on".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        assert!(has_call(&env.calls(), "direnv_allow(/r/.worktrees/task)"));
    }

    #[test]
    fn spawn_skips_direnv_allow_when_no_envrc_was_written() {
        // Most repos have no `.envrc`. Approving a path that has none
        // would just make direnv error, so the call is skipped.
        let env = FakeEnv {
            direnv_allow: Some("on".into()),
            copy_is_noop: true,
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        assert!(!has_call(&env.calls(), "direnv_allow("));
    }

    #[test]
    fn spawn_skips_direnv_allow_when_envrc_is_not_in_the_copy_list() {
        let env = FakeEnv {
            direnv_allow: Some("on".into()),
            copy_files: Some(".env.local".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        assert!(has_call(
            &env.calls(),
            "copy_into_worktree(/r,/r/.worktrees/task,.env.local)"
        ));
        assert!(!has_call(&env.calls(), "direnv_allow("));
    }

    #[test]
    fn spawn_uses_custom_repo_relative_worktree_dir() {
        let env = FakeEnv {
            worktree_dir: Some(".worktrees".into()),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect("spawn should succeed");
        let calls = env.calls();
        assert!(has_call(
            &calls,
            "worktree_add(/r,/r/.worktrees/task,agent/task)"
        ));
        assert!(has_call(&calls, "split_pane(%0,/r/.worktrees/task)"));
    }

    #[test]
    fn spawn_rejects_absolute_worktree_dir() {
        let env = FakeEnv {
            worktree_dir: Some("/tmp/worktrees".into()),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("worktree directory must be repo-relative"),
            "absolute worktree dir should be rejected before path allocation: {err}"
        );
        assert!(
            !has_call(&env.calls(), "worktree_add("),
            "spawn must not create a worktree with an absolute configured dir"
        );
    }

    #[test]
    fn spawn_rejects_parent_relative_worktree_dir() {
        let env = FakeEnv {
            worktree_dir: Some("../worktrees".into()),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("worktree directory must be repo-relative"),
            "parent-relative worktree dir should be rejected before path allocation: {err}"
        );
        assert!(
            !has_call(&env.calls(), "worktree_add("),
            "spawn must not create a worktree outside the repo"
        );
    }

    #[test]
    fn spawn_rolls_back_when_first_marker_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(0),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains(SPAWNED_OPTION),
            "error mentions ownership flag: {err}"
        );
        let calls = env.calls();
        assert!(
            has_call(&calls, "kill_pane(%1)"),
            "kill_pane rollback: {calls:?}"
        );
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree_remove rollback: {calls:?}"
        );
        assert!(
            !has_call(&calls, "send_command("),
            "send_command must not run after marker failure: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_when_middle_marker_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(2),
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains(SPAWNED_FROM_OPTION),
            "error mentions repo marker: {err}"
        );
        let calls = env.calls();
        assert!(has_call(&calls, "kill_pane(%1)"));
        assert!(!has_call(&calls, "kill_window("));
        assert!(has_call(&calls, "worktree_remove("));
        assert!(!has_call(&calls, "send_command("));
    }

    #[test]
    fn remove_runs_all_git_steps_before_kill_window() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect("remove should succeed");
        let calls = env.calls();
        let wt_idx = calls
            .iter()
            .position(|c| c.starts_with("worktree_remove"))
            .expect("worktree_remove called");
        let branch_idx = calls
            .iter()
            .position(|c| c.starts_with("branch_delete"))
            .expect("branch_delete called");
        let kill_idx = calls
            .iter()
            .position(|c| c.starts_with("kill_window"))
            .expect("kill_window called");
        assert!(
            wt_idx < branch_idx && branch_idx < kill_idx,
            "git cleanup must precede kill_window so a git failure \
             leaves the window as a retry handle: {calls:?}"
        );
    }

    #[test]
    fn remove_does_not_kill_window_when_worktree_remove_fails() {
        let env = FakeEnv {
            fail_worktree_remove: true,
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("worktree_remove failed"), "error: {err}");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "kill_window("),
            "kill_window must not run when git cleanup fails — the window is the only retry handle: {calls:?}"
        );
    }

    #[test]
    fn remove_skips_git_step_when_worktree_already_gone() {
        let env = FakeEnv {
            worktree_path_already_gone: Some(true),
            fail_worktree_remove: true,
            ..FakeEnv::default()
        };
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree)
            .expect("remove should still succeed via kill when worktree path is gone");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "worktree_remove("),
            "worktree_remove must be skipped when the path is already gone: {calls:?}"
        );
        assert!(has_call(&calls, "kill_window(@1)"));
    }

    #[test]
    fn remove_window_only_skips_worktree_remove() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowOnly).expect("remove should succeed");
        let calls = env.calls();
        assert!(has_call(&calls, "kill_window(@1)"));
        assert!(
            !has_call(&calls, "worktree_remove("),
            "WindowOnly must not touch worktree: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_when_send_command_fails() {
        let env = FakeEnv {
            fail_send_command: true,
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("send_command failed"),
            "primary error surfaced: {err}"
        );
        let calls = env.calls();
        assert!(
            has_call(&calls, "kill_pane(%1)"),
            "kill_pane rollback on send failure: {calls:?}"
        );
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree_remove rollback on send failure: {calls:?}"
        );
        assert!(
            has_call(&calls, "branch_delete(/r,agent/task)"),
            "branch_delete rollback on send failure: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_branch_when_marker_fails() {
        let env = FakeEnv {
            fail_set_option_at: Some(0),
            ..FakeEnv::default()
        };
        spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        let calls = env.calls();
        assert!(
            has_call(&calls, "branch_delete(/r,agent/task)"),
            "rollback must delete the branch `git worktree add -b` created: {calls:?}"
        );
    }

    #[test]
    fn spawn_rolls_back_branch_when_split_fails() {
        #[derive(Default)]
        struct SplitFailingEnv(FakeEnv);
        impl SpawnEnv for SplitFailingEnv {
            fn branch_prefix(&self) -> Option<String> {
                self.0.branch_prefix()
            }
            fn worktree_dir(&self) -> Option<String> {
                self.0.worktree_dir()
            }
            fn copy_files(&self) -> Option<String> {
                self.0.copy_files()
            }
            fn copy_into_worktree(&self, r: &str, w: &str, n: &str) -> Result<bool, String> {
                self.0.copy_into_worktree(r, w, n)
            }
            fn direnv_allow_option(&self) -> Option<String> {
                self.0.direnv_allow_option()
            }
            fn direnv_allow(&self, w: &str) -> Result<(), String> {
                self.0.direnv_allow(w)
            }
            fn branch_is_free(&self, r: &str, b: &str) -> bool {
                self.0.branch_is_free(r, b)
            }
            fn branch_exists(&self, r: &str, b: &str) -> bool {
                self.0.branch_exists(r, b)
            }
            fn worktree_path_is_free(&self, p: &Path) -> bool {
                self.0.worktree_path_is_free(p)
            }
            fn worktree_path_exists(&self, p: &str) -> bool {
                self.0.worktree_path_exists(p)
            }
            fn worktree_add(&self, r: &str, p: &str, b: &str) -> Result<(), String> {
                self.0.worktree_add(r, p, b)
            }
            fn worktree_remove(&self, r: &str, p: &str) -> Result<(), String> {
                self.0.worktree_remove(r, p)
            }
            fn branch_delete(&self, r: &str, b: &str) -> Result<(), String> {
                self.0.branch_delete(r, b)
            }
            fn split_target(&self, origin: &str) -> Result<String, String> {
                self.0.split_target(origin)
            }
            fn split_pane(
                &self,
                _target: &str,
                _cwd: &str,
                _detached: bool,
            ) -> Result<String, String> {
                self.0.log("split_pane(fail)".into());
                Err("split_pane failed".into())
            }
            fn kill_pane(&self, pane: &str) -> Result<(), String> {
                self.0.kill_pane(pane)
            }
            fn kill_window(&self, w: &str) -> Result<(), String> {
                self.0.kill_window(w)
            }
            fn set_pane_option(&self, p: &str, k: &str, v: &str) -> Result<(), String> {
                self.0.set_pane_option(p, k, v)
            }
            fn send_command(&self, t: &str, c: &str) -> Result<(), String> {
                self.0.send_command(t, c)
            }
            fn display_message(&self, p: &str, t: &str) -> String {
                self.0.display_message(p, t)
            }
        }

        let env = SplitFailingEnv::default();
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(err.contains("split_pane failed"));
        let calls = env.0.calls();
        assert!(
            !has_call(&calls, "kill_window("),
            "no pane was ever created: {calls:?}"
        );
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree must be cleaned up after split failure: {calls:?}"
        );
        assert!(
            has_call(&calls, "branch_delete(/r,agent/task)"),
            "branch must be deleted after split failure: {calls:?}"
        );
    }

    #[test]
    fn spawn_preserves_checkout_when_rollback_cannot_kill_pane() {
        let env = FakeEnv {
            fail_set_option_at: Some(0),
            fail_kill_pane: true,
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("rollback incomplete"),
            "rollback failure surfaced: {err}"
        );
        assert!(
            err.contains("kill_pane"),
            "rollback error names kill_pane: {err}"
        );
        assert!(!has_call(&env.calls(), "worktree_remove("));
        assert!(!has_call(&env.calls(), "branch_delete("));
    }

    #[test]
    fn remove_split_never_kills_the_shared_window() {
        for mode in [RemoveMode::WindowOnly, RemoveMode::WindowAndWorktree] {
            let env = FakeEnv {
                display_output: Some("1\n/r\n/r/.worktrees/task\nagent/task\n@1\npane\n".into()),
                ..FakeEnv::default()
            };
            remove_with(&env, "%1", mode).unwrap();
            let calls = env.calls();
            assert!(has_call(&calls, "kill_pane(%1)"));
            assert!(!has_call(&calls, "kill_window("));
            assert_eq!(
                has_call(&calls, "worktree_remove("),
                mode == RemoveMode::WindowAndWorktree
            );
            assert_eq!(
                has_call(&calls, "branch_delete("),
                mode == RemoveMode::WindowAndWorktree
            );
            if mode == RemoveMode::WindowAndWorktree {
                assert!(calls.last().unwrap().starts_with("kill_pane("));
            }
        }
    }

    #[test]
    fn remove_split_retains_retry_handle_on_git_failure() {
        for branch_failure in [false, true] {
            let env = FakeEnv {
                display_output: Some("1\n/r\n/r/.worktrees/task\nagent/task\n@1\npane\n".into()),
                fail_worktree_remove: !branch_failure,
                fail_branch_delete: branch_failure,
                ..FakeEnv::default()
            };
            assert!(remove_with(&env, "%1", RemoveMode::WindowAndWorktree).is_err());
            assert!(!has_call(&env.calls(), "kill_pane("));
            assert!(!has_call(&env.calls(), "kill_window("));
        }
    }

    #[test]
    fn spawn_surfaces_rollback_failure_when_worktree_remove_also_fails() {
        let env = FakeEnv {
            fail_send_command: true,
            fail_worktree_remove: true,
            ..FakeEnv::default()
        };
        let err = spawn_with(&env, &sample_req()).expect_err("spawn must fail");
        assert!(
            err.contains("send_command failed"),
            "primary error surfaced: {err}"
        );
        assert!(
            err.contains("rollback incomplete"),
            "rollback failure surfaced: {err}"
        );
        assert!(
            err.contains("worktree_remove"),
            "rollback error names worktree_remove: {err}"
        );
    }

    #[test]
    fn remove_rejects_pane_missing_spawned_marker() {
        let env = FakeEnv {
            display_output: Some("\n/r\n/r/.worktrees/task\nagent/task\n@1\n".into()),
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("not created by sidebar spawn"));
        assert!(!has_call(&env.calls(), "kill_window("));
    }

    #[test]
    fn remove_deletes_branch_between_worktree_and_kill_window() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect("remove should succeed");
        let calls = env.calls();
        let wt_idx = calls
            .iter()
            .position(|c| c.starts_with("worktree_remove"))
            .expect("worktree_remove called");
        let branch_idx = calls
            .iter()
            .position(|c| c.starts_with("branch_delete"))
            .expect("branch_delete called");
        let kill_idx = calls
            .iter()
            .position(|c| c.starts_with("kill_window"))
            .expect("kill_window called");
        assert!(
            wt_idx < branch_idx && branch_idx < kill_idx,
            "expected worktree → branch → kill order: {calls:?}"
        );
        assert!(has_call(&calls, "branch_delete(/r,agent/task)"));
    }

    #[test]
    fn remove_window_only_does_not_delete_branch() {
        let env = FakeEnv::default();
        remove_with(&env, "%1", RemoveMode::WindowOnly).expect("remove should succeed");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "branch_delete("),
            "WindowOnly must not touch branch: {calls:?}"
        );
    }

    #[test]
    fn remove_rejects_pane_with_empty_branch_marker() {
        let env = FakeEnv {
            display_output: Some("1\n/r\n/r/.worktrees/task\n\n@1\n".into()),
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("branch is unset"), "error: {err}");
        assert!(!has_call(&env.calls(), "worktree_remove("));
        assert!(!has_call(&env.calls(), "kill_window("));
    }

    #[test]
    fn remove_does_not_kill_window_when_branch_delete_fails() {
        let env = FakeEnv {
            fail_branch_delete: true,
            ..FakeEnv::default()
        };
        let err =
            remove_with(&env, "%1", RemoveMode::WindowAndWorktree).expect_err("remove must fail");
        assert!(err.contains("branch_delete failed"), "error: {err}");
        let calls = env.calls();
        assert!(
            has_call(&calls, "worktree_remove("),
            "worktree_remove ran first: {calls:?}"
        );
        assert!(
            !has_call(&calls, "kill_window("),
            "kill_window must not run when branch_delete fails — \
             the window is the only retry handle for the orphaned \
             branch: {calls:?}"
        );
    }

    #[test]
    fn remove_skips_branch_delete_when_branch_already_gone() {
        // Simulates a retry after a previous partial success already
        // dropped the branch (e.g. branch_delete succeeded but
        // kill_window then failed on a prior attempt). The flow must
        // converge instead of re-running `git branch -D` and erroring
        // on a missing ref.
        let env = FakeEnv {
            worktree_path_already_gone: Some(true),
            branch_already_gone: Some(true),
            fail_branch_delete: true,
            ..FakeEnv::default()
        };
        remove_with(&env, "%1", RemoveMode::WindowAndWorktree)
            .expect("retry should converge once git state is gone");
        let calls = env.calls();
        assert!(
            !has_call(&calls, "branch_delete("),
            "branch_delete must be skipped when branch is gone: {calls:?}"
        );
        assert!(
            !has_call(&calls, "worktree_remove("),
            "worktree_remove must also stay skipped: {calls:?}"
        );
        assert!(has_call(&calls, "kill_window(@1)"));
    }
}
