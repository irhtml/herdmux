use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use indexmap::IndexMap;

use crate::git::run_git;
use crate::tmux::PaneInfo;

/// Per-pane git metadata resolved from the pane's working directory.
#[derive(Debug, Clone, Default)]
pub struct PaneGitInfo {
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub is_worktree: bool,
    pub worktree_name: Option<String>,
}

/// A group of panes working in the same repository (or directory).
#[derive(Debug, Clone)]
pub struct RepoGroup {
    /// Display name: repo directory basename, or raw path for non-git
    pub name: String,
    /// Whether any pane in the group belongs to the focused (active) window
    pub has_focus: bool,
    /// Panes in this group, with their git info
    pub panes: Vec<(PaneInfo, PaneGitInfo)>,
}

/// Resolve git info for a single pane path.
pub fn resolve_pane_git_info(path: &str) -> PaneGitInfo {
    if path.is_empty() {
        return PaneGitInfo::default();
    }

    // Single git call for all three values (one line per arg)
    let combined = run_git(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
            "--git-common-dir",
            "--git-dir",
        ],
    );
    let (branch, git_common_dir, git_dir) = match combined {
        Some(output) => {
            let mut lines = output.lines();
            let b = lines.next().map(|s| s.to_string());
            let c = lines.next().map(|s| s.to_string());
            let d = lines.next().map(|s| s.to_string());
            (b, c, d)
        }
        None => (None, None, None),
    };

    let is_worktree = match (&git_common_dir, &git_dir) {
        (Some(common), Some(dir)) => {
            let common_path = resolve_git_path(path, common);
            let dir_path = resolve_git_path(path, dir);
            common_path != dir_path
        }
        _ => false,
    };

    // --git-common-dir returns the .git dir of the main worktree;
    // its parent is the repo root, so worktrees share the same group key.
    let repo_root = git_common_dir
        .as_ref()
        .and_then(|common| {
            let abs = resolve_git_path(path, common);
            abs.parent().map(|p| p.to_string_lossy().to_string())
        })
        .or_else(|| run_git(path, &["rev-parse", "--show-toplevel"]));

    PaneGitInfo {
        repo_root,
        branch,
        is_worktree,
        worktree_name: None,
    }
}

/// How long a path's git info is reused before git is asked again. The
/// sidebar groups panes every second, and a `git rev-parse` per distinct
/// path each time adds up; a branch switch shows within this delay.
const GIT_INFO_TTL: Duration = Duration::from_secs(3);

/// Git info per pane path, kept between [`group_panes_by_repo_cached`]
/// calls for [`GIT_INFO_TTL`].
#[derive(Debug, Default)]
pub struct GitInfoCache {
    entries: HashMap<String, (Instant, PaneGitInfo)>,
}

impl GitInfoCache {
    /// The cached info for `path` while it is fresh at `now`, otherwise
    /// whatever `resolve` returns, which is then cached.
    fn get_or_resolve(
        &mut self,
        path: &str,
        now: Instant,
        resolve: impl FnOnce(&str) -> PaneGitInfo,
    ) -> PaneGitInfo {
        if let Some((at, info)) = self.entries.get(path)
            && now.duration_since(*at) < GIT_INFO_TTL
        {
            return info.clone();
        }
        let info = resolve(path);
        self.entries.insert(path.to_string(), (now, info.clone()));
        info
    }

    /// Forget paths no pane is in any more.
    fn retain_paths(&mut self, seen: &HashSet<&str>) {
        self.entries.retain(|path, _| seen.contains(path.as_str()));
    }
}

/// Group all panes across all sessions by repo root.
/// Returns groups sorted alphabetically by display name (case-insensitive),
/// so the order is stable regardless of which pane is encountered first.
pub fn group_panes_by_repo(sessions: &[crate::tmux::SessionInfo]) -> Vec<RepoGroup> {
    group_panes_by_repo_cached(sessions, &mut GitInfoCache::default())
}

/// [`group_panes_by_repo`] with the per-path git info reused from `cache`
/// across calls.
pub fn group_panes_by_repo_cached(
    sessions: &[crate::tmux::SessionInfo],
    cache: &mut GitInfoCache,
) -> Vec<RepoGroup> {
    let mut groups: IndexMap<String, RepoGroup> = IndexMap::new();
    let now = Instant::now();
    let mut seen: HashSet<&str> = HashSet::new();

    for session in sessions {
        for window in &session.windows {
            for pane in &window.panes {
                // The base git info costs a git command per path, so it
                // comes from the cache while fresh.
                seen.insert(pane.path.as_str());
                let mut git_info = cache.get_or_resolve(&pane.path, now, resolve_pane_git_info);

                // Override with hook-provided worktree info (Claude Code
                // provides this; Codex does not, so the git-command base
                // remains as fallback).
                if !pane.worktree.name.is_empty() {
                    git_info.worktree_name = Some(pane.worktree.name.clone());
                    git_info.is_worktree = true;
                }
                if !pane.worktree.branch.is_empty() {
                    git_info.branch = Some(pane.worktree.branch.clone());
                    git_info.is_worktree = true;
                }

                let group_key = match &git_info.repo_root {
                    Some(root) => root.clone(),
                    None => pane.path.clone(),
                };

                let display_name = group_key
                    .rsplit('/')
                    .next()
                    .unwrap_or(&group_key)
                    .to_string();

                let has_focus = window.window_active && pane.pane_active;

                let group = groups.entry(group_key).or_insert_with(|| RepoGroup {
                    name: display_name,
                    has_focus: false,
                    panes: Vec::new(),
                });

                if has_focus {
                    group.has_focus = true;
                }

                group.panes.push((pane.clone(), git_info));
            }
        }
    }

    cache.retain_paths(&seen);

    let mut result: Vec<RepoGroup> = groups.into_values().collect();
    result.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    result
}

/// Resolve a possibly-relative git path to an absolute canonical path.
fn resolve_git_path(base: &str, git_path: &str) -> std::path::PathBuf {
    let p = if std::path::Path::new(git_path).is_absolute() {
        std::path::PathBuf::from(git_path)
    } else {
        std::path::PathBuf::from(base).join(git_path)
    };
    p.canonicalize().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_on(branch: &str) -> PaneGitInfo {
        PaneGitInfo {
            branch: Some(branch.into()),
            ..PaneGitInfo::default()
        }
    }

    #[test]
    fn git_info_cache_reuses_a_fresh_entry_and_resolves_again_once_stale() {
        let mut cache = GitInfoCache::default();
        let start = Instant::now();
        let first = cache.get_or_resolve("/repo", start, |_| info_on("main"));
        assert_eq!(first.branch.as_deref(), Some("main"));

        let fresh = cache.get_or_resolve("/repo", start + GIT_INFO_TTL / 2, |_| {
            panic!("a fresh entry must not ask git")
        });
        assert_eq!(fresh.branch.as_deref(), Some("main"));

        let stale = cache.get_or_resolve("/repo", start + GIT_INFO_TTL, |_| info_on("feature"));
        assert_eq!(stale.branch.as_deref(), Some("feature"));
    }

    #[test]
    fn git_info_cache_forgets_paths_without_panes() {
        let mut cache = GitInfoCache::default();
        let now = Instant::now();
        cache.get_or_resolve("/kept", now, |_| info_on("main"));
        cache.get_or_resolve("/gone", now, |_| info_on("main"));

        cache.retain_paths(&HashSet::from(["/kept"]));

        cache.get_or_resolve("/kept", now, |_| panic!("still cached"));
        let again = cache.get_or_resolve("/gone", now, |_| info_on("other"));
        assert_eq!(again.branch.as_deref(), Some("other"));
    }

    /// A throwaway main checkout with one commit, so the tests do not depend
    /// on where this source tree is checked out (a linked worktree is not a
    /// main checkout).
    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ][..],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }
        dir
    }

    #[test]
    fn resolve_git_info_returns_none_for_empty_path() {
        let info = resolve_pane_git_info("");
        assert!(info.branch.is_none());
        assert!(!info.is_worktree);
        assert!(info.repo_root.is_none());
    }

    #[test]
    fn resolve_git_info_for_real_repo() {
        let repo = temp_repo();
        let repo_path = repo.path().to_str().unwrap();
        let info = resolve_pane_git_info(repo_path);
        assert!(info.repo_root.is_some(), "should detect git repo");
        assert!(info.branch.is_some(), "should detect branch");
        let root = info.repo_root.unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let repo_dir = std::fs::canonicalize(repo.path()).unwrap();
        assert_eq!(root, repo_dir, "repo root should be the repo dir");
    }

    #[test]
    fn worktree_and_main_share_same_repo_root() {
        // Both main and worktree should resolve to the same repo_root
        // We can only test the main worktree here, but verify the logic is consistent
        let repo = temp_repo();
        let info = resolve_pane_git_info(repo.path().to_str().unwrap());
        assert!(
            !info.is_worktree,
            "main checkout should not be detected as worktree"
        );
        assert!(info.repo_root.is_some());
    }

    // ─── resolve_git_path tests ─────────────────────────────────────

    #[test]
    fn resolve_git_path_absolute() {
        let result = resolve_git_path("/base", "/absolute/path");
        assert_eq!(result, std::path::PathBuf::from("/absolute/path"));
    }

    #[test]
    fn resolve_git_path_relative() {
        let result = resolve_git_path("/base/dir", "relative");
        assert_eq!(result, std::path::PathBuf::from("/base/dir/relative"));
    }

    // ─── group_panes_by_repo tests ──────────────────────────────────

    fn test_pane(id: &str, path: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: crate::tmux::PaneStatus::Running,
            attention: false,
            agent: crate::tmux::AgentType::Claude,
            path: path.into(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: crate::tmux::PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: crate::tmux::WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            pane_desc: String::new(),
            sidebar_spawned: false,
            spawned_by: None,
            pinned: false,
            activity_at: None,
            unread: false,
            window_name: String::new(),
            tmux_session: String::new(),
            bg_shell_cmd: None,
        }
    }

    fn test_window(panes: Vec<PaneInfo>, active: bool) -> crate::tmux::WindowInfo {
        crate::tmux::WindowInfo {
            window_id: "@0".into(),
            window_name: "test".into(),
            window_active: active,
            auto_rename: false,
            panes,
        }
    }

    fn test_session(windows: Vec<crate::tmux::WindowInfo>) -> crate::tmux::SessionInfo {
        crate::tmux::SessionInfo {
            session_name: "main".into(),
            windows,
        }
    }

    #[test]
    fn group_panes_empty_sessions() {
        let groups = group_panes_by_repo(&[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn group_panes_same_repo() {
        // Two panes in the same real repo should be grouped together
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let pane1 = test_pane("%1", manifest_dir);
        let pane2 = test_pane("%2", manifest_dir);

        let sessions = vec![test_session(vec![test_window(vec![pane1, pane2], true)])];
        let groups = group_panes_by_repo(&sessions);

        assert_eq!(groups.len(), 1, "same repo path should produce one group");
        assert_eq!(groups[0].panes.len(), 2);
        assert_eq!(groups[0].panes[0].0.pane_id, "%1");
        assert_eq!(groups[0].panes[1].0.pane_id, "%2");
    }

    #[test]
    fn group_panes_non_git_path_uses_raw_path() {
        // A non-git path should use the raw path as the group key
        let pane = test_pane("%1", "/tmp/no-git-here");

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "no-git-here");
    }

    #[test]
    fn group_panes_display_name_is_basename() {
        let repo = temp_repo();
        let repo_path = std::fs::canonicalize(repo.path()).unwrap();
        let pane = test_pane("%1", repo_path.to_str().unwrap());

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions);

        assert_eq!(groups.len(), 1);
        let expected_name = repo_path.file_name().unwrap().to_string_lossy();
        assert_eq!(
            groups[0].name, expected_name,
            "display name should be repo basename"
        );
    }

    #[test]
    fn group_panes_has_focus_from_active_window_and_pane() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut pane = test_pane("%1", manifest_dir);
        pane.pane_active = true;

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions);

        assert!(
            groups[0].has_focus,
            "active pane in active window should set has_focus"
        );
    }

    #[test]
    fn group_panes_no_focus_when_window_inactive() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut pane = test_pane("%1", manifest_dir);
        pane.pane_active = true;

        let sessions = vec![test_session(vec![test_window(vec![pane], false)])]; // window_active=false
        let groups = group_panes_by_repo(&sessions);

        assert!(
            !groups[0].has_focus,
            "active pane in inactive window should not set has_focus"
        );
    }

    #[test]
    fn group_panes_empty_path_pane() {
        let pane = test_pane("%1", "");

        let sessions = vec![test_session(vec![test_window(vec![pane], true)])];
        let groups = group_panes_by_repo(&sessions);

        // Empty path pane should still be grouped (by empty key)
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn group_panes_multiple_sessions() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let pane1 = test_pane("%1", manifest_dir);
        let pane2 = test_pane("%2", "/tmp/other-project");

        let sessions = vec![
            test_session(vec![test_window(vec![pane1], true)]),
            crate::tmux::SessionInfo {
                session_name: "other".into(),
                windows: vec![test_window(vec![pane2], false)],
            },
        ];
        let groups = group_panes_by_repo(&sessions);

        assert_eq!(
            groups.len(),
            2,
            "different repos across sessions should produce separate groups"
        );
    }

    #[test]
    fn group_panes_same_repo_across_sessions_merge_into_one_group() {
        // Regression for the `state.sessions` field removal: panes that
        // live in different tmux sessions but share the same repo path
        // must still collapse into a single `RepoGroup`. This is what
        // makes the sidebar usable across multi-session workflows.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let pane_session_a = test_pane("%1", manifest_dir);
        let pane_session_b = test_pane("%2", manifest_dir);

        let sessions = vec![
            crate::tmux::SessionInfo {
                session_name: "alpha".into(),
                windows: vec![test_window(vec![pane_session_a], true)],
            },
            crate::tmux::SessionInfo {
                session_name: "beta".into(),
                windows: vec![test_window(vec![pane_session_b], false)],
            },
        ];
        let groups = group_panes_by_repo(&sessions);

        assert_eq!(
            groups.len(),
            1,
            "panes in the same repo across sessions must merge into one group"
        );
        assert_eq!(groups[0].panes.len(), 2);
        let pane_ids: Vec<&str> = groups[0]
            .panes
            .iter()
            .map(|(p, _)| p.pane_id.as_str())
            .collect();
        assert!(pane_ids.contains(&"%1"));
        assert!(pane_ids.contains(&"%2"));
    }

    #[test]
    fn group_panes_sorted_by_name_case_insensitive() {
        // Groups should be sorted alphabetically regardless of encounter order
        let pane1 = test_pane("%1", "/tmp/zzz");
        let pane2 = test_pane("%2", "/tmp/Aaa");
        let pane3 = test_pane("%3", "/tmp/mmm");
        let pane4 = test_pane("%4", "/tmp/zzz");

        let sessions = vec![test_session(vec![test_window(
            vec![pane1, pane2, pane3, pane4],
            true,
        )])];
        let groups = group_panes_by_repo(&sessions);

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].name, "Aaa");
        assert_eq!(groups[1].name, "mmm");
        assert_eq!(groups[2].name, "zzz");
        assert_eq!(groups[2].panes.len(), 2, "zzz should have 2 panes");
    }
}
