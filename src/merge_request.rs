//! Open merge requests for the branches checked out in a tmux session.
//!
//! Every pane of the session contributes its repo's `origin` remote and
//! current branch. GitLab remotes are looked up with `glab mr list`, GitHub
//! remotes with `gh pr list`, both filtered to that source branch. Results
//! go into a cache file shared by every sidebar instance, so opening the
//! tab in another window of the session shows them without refetching.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::git::{normalize_git_url, run_git, run_with_timeout};

/// How long a branch's lookup stays fresh in the shared cache.
pub const MR_CACHE_TTL_SECS: u64 = 300;

/// Cache entries untouched for this long are dropped on the next write, so
/// branches that were deleted long ago do not accumulate.
const MR_CACHE_PRUNE_SECS: u64 = 7 * 24 * 60 * 60;

/// Upper bound on one `glab` / `gh` call, so a hung CLI cannot stall the
/// poll thread.
const CLI_TIMEOUT: Duration = Duration::from_secs(10);

/// Lookups that run at the same time during one poll pass.
const MAX_PARALLEL_FETCHES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Forge {
    GitHub,
    GitLab,
}

impl Forge {
    /// Classify a remote URL by host. Self-hosted GitLab counts when its
    /// host name contains `gitlab`; anything else (Bitbucket, plain SSH
    /// servers) has no lookup.
    pub fn from_remote(remote: &str) -> Option<Self> {
        let host = remote_host(remote)?;
        if host == "github.com" {
            Some(Self::GitHub)
        } else if host.contains("gitlab") {
            Some(Self::GitLab)
        } else {
            None
        }
    }

    /// Prefix the forge's web UI puts before an MR / PR number.
    pub fn number_prefix(self) -> char {
        match self {
            Self::GitHub => '#',
            Self::GitLab => '!',
        }
    }
}

/// A branch checked out in at least one pane of the session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BranchRef {
    /// Repo name for display: the last path segment of the remote.
    pub repo: String,
    pub branch: String,
    /// `origin` URL exactly as git prints it.
    pub remote: String,
    pub forge: Forge,
}

/// The per-MR fields stored in the shared cache.
#[derive(Debug, Clone, PartialEq)]
pub struct MrSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
}

/// One open MR, ready to render.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeRequest {
    pub repo: String,
    pub branch: String,
    pub forge: Forge,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
}

impl MergeRequest {
    /// `!123` for GitLab, `#123` for GitHub.
    pub fn label(&self) -> String {
        format!("{}{}", self.forge.number_prefix(), self.number)
    }
}

/// Working directory of every non-sidebar pane in the session that
/// contains `pane_id`. Prefers the agent-reported `@pane_cwd` over
/// `pane_current_path`, as the main pane query does.
pub fn session_pane_paths(pane_id: &str) -> Vec<String> {
    let Some(output) = crate::tmux::run_tmux(&[
        "list-panes",
        "-s",
        "-t",
        pane_id,
        "-F",
        "#{@pane_role}|#{@pane_cwd}|#{pane_current_path}",
    ]) else {
        return Vec::new();
    };
    parse_session_pane_paths(&output)
}

pub(crate) fn parse_session_pane_paths(output: &str) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for line in output.lines() {
        let mut fields = line.splitn(3, '|');
        let (Some(role), Some(cwd), Some(current)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if role == "sidebar" {
            continue;
        }
        let path = if cwd.is_empty() { current } else { cwd };
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
    paths.into_iter().collect()
}

/// Resolve each path to its repo's remote and branch. Paths outside a git
/// repo, on a detached HEAD, without an `origin`, or on a forge without a
/// lookup are skipped. Panes on the same remote and branch collapse into
/// one entry.
pub fn resolve_branches<F>(paths: &[String], mut resolve: F) -> Vec<BranchRef>
where
    F: FnMut(&str) -> Option<(String, String)>,
{
    let mut branches = BTreeSet::new();
    for path in paths {
        let Some((remote, branch)) = resolve(path) else {
            continue;
        };
        if branch.is_empty() || branch == "HEAD" {
            continue;
        }
        let Some(forge) = Forge::from_remote(&remote) else {
            continue;
        };
        branches.insert(BranchRef {
            repo: repo_name(&remote),
            branch,
            remote,
            forge,
        });
    }
    branches.into_iter().collect()
}

/// `(origin URL, branch)` of the repo at `path`.
pub fn git_remote_and_branch(path: &str) -> Option<(String, String)> {
    let branch = run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let remote = run_git(path, &["remote", "get-url", "origin"])?;
    Some((remote, branch))
}

/// Lowercase host of an `https://`, `ssh://` or scp-style (`git@host:path`)
/// remote, without user or port.
fn remote_host(remote: &str) -> Option<String> {
    let normalized = normalize_git_url(remote);
    let rest = ["https://", "http://", "ssh://"]
        .iter()
        .find_map(|scheme| normalized.strip_prefix(scheme))?;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn repo_name(remote: &str) -> String {
    let normalized = normalize_git_url(remote);
    normalized
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(&normalized)
        .to_string()
}

/// Open MRs whose source branch is `branch.branch`. `None` when the CLI is
/// missing, fails, or times out, so the caller keeps any older result
/// instead of caching an empty list.
pub fn fetch_open_mrs(branch: &BranchRef) -> Option<Vec<MrSummary>> {
    match branch.forge {
        Forge::GitLab => {
            let mut cmd = Command::new("glab");
            cmd.args([
                "mr",
                "list",
                "-R",
                &branch.remote,
                "--source-branch",
                &branch.branch,
                "-F",
                "json",
            ]);
            parse_glab_json(&run_with_timeout(&mut cmd, CLI_TIMEOUT)?)
        }
        Forge::GitHub => {
            let slug = normalize_git_url(&branch.remote);
            let slug = slug.strip_prefix("https://").unwrap_or(&slug);
            let mut cmd = Command::new("gh");
            cmd.args([
                "pr",
                "list",
                "-R",
                slug,
                "--head",
                &branch.branch,
                "--state",
                "open",
                "--json",
                "number,title,url,isDraft",
            ]);
            parse_gh_json(&run_with_timeout(&mut cmd, CLI_TIMEOUT)?)
        }
    }
}

/// Parse `glab mr list -F json`. GitLab keeps the `Draft:` prefix in the
/// title; it is dropped here because the row marks drafts on its own.
pub(crate) fn parse_glab_json(text: &str) -> Option<Vec<MrSummary>> {
    let items: Vec<Value> = serde_json::from_str(text).ok()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let draft = item["draft"].as_bool().unwrap_or(false);
                let title = item["title"].as_str()?;
                Some(MrSummary {
                    number: item["iid"].as_u64()?,
                    title: if draft {
                        strip_draft_prefix(title)
                    } else {
                        title.to_string()
                    },
                    url: item["web_url"].as_str()?.to_string(),
                    draft,
                })
            })
            .collect(),
    )
}

/// Parse `gh pr list --json number,title,url,isDraft`.
pub(crate) fn parse_gh_json(text: &str) -> Option<Vec<MrSummary>> {
    let items: Vec<Value> = serde_json::from_str(text).ok()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                Some(MrSummary {
                    number: item["number"].as_u64()?,
                    title: item["title"].as_str()?.to_string(),
                    url: item["url"].as_str()?.to_string(),
                    draft: item["isDraft"].as_bool().unwrap_or(false),
                })
            })
            .collect(),
    )
}

fn strip_draft_prefix(title: &str) -> String {
    let lower = title.to_ascii_lowercase();
    for prefix in ["draft:", "[draft]", "(draft)"] {
        if lower.starts_with(prefix) {
            return title[prefix.len()..].trim_start().to_string();
        }
    }
    title.to_string()
}

#[derive(Debug, Clone, PartialEq)]
struct CacheEntry {
    fetched_at: u64,
    mrs: Vec<MrSummary>,
}

/// Lookup results keyed by remote, then branch. Persisted as JSON at
/// [`MrCache::default_path`] and shared by every sidebar instance.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MrCache {
    entries: BTreeMap<String, BTreeMap<String, CacheEntry>>,
}

impl MrCache {
    /// `${XDG_CACHE_HOME:-~/.cache}/herdmux/merge-requests.json`.
    pub fn default_path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
        };
        Some(base.join("herdmux").join("merge-requests.json"))
    }

    /// A missing or unreadable file is an empty cache.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| Self::from_json(&text))
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::fs_util::write_private_atomic(path, self.to_json().to_string().as_bytes())
    }

    fn get(&self, branch: &BranchRef) -> Option<&CacheEntry> {
        self.entries.get(&branch.remote)?.get(&branch.branch)
    }

    pub fn is_fresh(&self, branch: &BranchRef, now: u64) -> bool {
        self.get(branch)
            .is_some_and(|e| now.saturating_sub(e.fetched_at) < MR_CACHE_TTL_SECS)
    }

    pub fn insert(&mut self, branch: &BranchRef, now: u64, mrs: Vec<MrSummary>) {
        self.entries
            .entry(branch.remote.clone())
            .or_default()
            .insert(
                branch.branch.clone(),
                CacheEntry {
                    fetched_at: now,
                    mrs,
                },
            );
    }

    pub fn prune(&mut self, now: u64) {
        for branches in self.entries.values_mut() {
            branches.retain(|_, e| now.saturating_sub(e.fetched_at) < MR_CACHE_PRUNE_SECS);
        }
        self.entries.retain(|_, branches| !branches.is_empty());
    }

    /// Cached MRs for `branches`, in branch order then MR number.
    /// Branches without an entry contribute nothing.
    pub fn merge_requests(&self, branches: &[BranchRef]) -> Vec<MergeRequest> {
        let mut out = Vec::new();
        for branch in branches {
            let Some(entry) = self.get(branch) else {
                continue;
            };
            for mr in &entry.mrs {
                out.push(MergeRequest {
                    repo: branch.repo.clone(),
                    branch: branch.branch.clone(),
                    forge: branch.forge,
                    number: mr.number,
                    title: mr.title.clone(),
                    url: mr.url.clone(),
                    draft: mr.draft,
                });
            }
        }
        out
    }

    fn to_json(&self) -> Value {
        let mut remotes = Map::new();
        for (remote, branches) in &self.entries {
            let mut by_branch = Map::new();
            for (branch, entry) in branches {
                let mrs: Vec<Value> = entry
                    .mrs
                    .iter()
                    .map(|mr| {
                        json!({
                            "number": mr.number,
                            "title": mr.title,
                            "url": mr.url,
                            "draft": mr.draft,
                        })
                    })
                    .collect();
                by_branch.insert(
                    branch.clone(),
                    json!({ "fetched_at": entry.fetched_at, "mrs": mrs }),
                );
            }
            remotes.insert(remote.clone(), Value::Object(by_branch));
        }
        Value::Object(remotes)
    }

    fn from_json(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let mut cache = Self::default();
        for (remote, branches) in value.as_object()? {
            let Some(branches) = branches.as_object() else {
                continue;
            };
            for (branch, entry) in branches {
                let Some(fetched_at) = entry["fetched_at"].as_u64() else {
                    continue;
                };
                let mrs = entry["mrs"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|mr| {
                                Some(MrSummary {
                                    number: mr["number"].as_u64()?,
                                    title: mr["title"].as_str()?.to_string(),
                                    url: mr["url"].as_str()?.to_string(),
                                    draft: mr["draft"].as_bool().unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                cache
                    .entries
                    .entry(remote.clone())
                    .or_default()
                    .insert(branch.clone(), CacheEntry { fetched_at, mrs });
            }
        }
        Some(cache)
    }
}

/// Open an `http(s)` URL with the system opener (`open` on macOS,
/// `xdg-open` elsewhere). Other schemes are ignored so cache contents can
/// never launch a local file. A detached thread reaps the opener.
pub fn open_url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Ok(mut child) = Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Run `fetch` for every branch, at most [`MAX_PARALLEL_FETCHES`] at a
/// time. Results keep the input order.
pub fn fetch_all<F>(branches: &[BranchRef], fetch: F) -> Vec<Option<Vec<MrSummary>>>
where
    F: Fn(&BranchRef) -> Option<Vec<MrSummary>> + Sync,
{
    let mut results = Vec::with_capacity(branches.len());
    for chunk in branches.chunks(MAX_PARALLEL_FETCHES) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|branch| scope.spawn(|| fetch(branch)))
                .collect();
            for handle in handles {
                results.push(handle.join().ok().flatten());
            }
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(remote: &str, name: &str) -> BranchRef {
        BranchRef {
            repo: repo_name(remote),
            branch: name.into(),
            remote: remote.into(),
            forge: Forge::from_remote(remote).unwrap(),
        }
    }

    fn summary(number: u64) -> MrSummary {
        MrSummary {
            number,
            title: format!("MR {number}"),
            url: format!("https://gitlab.com/g/r/-/merge_requests/{number}"),
            draft: false,
        }
    }

    #[test]
    fn forge_from_remote_classifies_hosts() {
        assert_eq!(
            Forge::from_remote("git@gitlab.com:acme/web/storefront.git"),
            Some(Forge::GitLab)
        );
        assert_eq!(
            Forge::from_remote("https://gitlab.example.com/team/app.git"),
            Some(Forge::GitLab)
        );
        assert_eq!(
            Forge::from_remote("git@github.com:irhtml/herdmux.git"),
            Some(Forge::GitHub)
        );
        assert_eq!(
            Forge::from_remote("ssh://git@gitlab.example.com:2222/team/app.git"),
            Some(Forge::GitLab)
        );
        assert_eq!(
            Forge::from_remote("git@bitbucket.org:acme/configs.git"),
            None
        );
        assert_eq!(Forge::from_remote("/srv/git/local.git"), None);
    }

    #[test]
    fn repo_name_is_last_remote_segment() {
        assert_eq!(
            repo_name("git@gitlab.com:acme/web/storefront.git"),
            "storefront"
        );
        assert_eq!(repo_name("https://github.com/irhtml/herdmux"), "herdmux");
    }

    #[test]
    fn session_pane_paths_skip_sidebars_and_prefer_agent_cwd() {
        let output = "sidebar||/home/u/a\n\
                      |/home/u/b/sub|/home/u/b\n\
                      ||/home/u/a\n\
                      ||/home/u/a\n\
                      ||\n";
        assert_eq!(
            parse_session_pane_paths(output),
            vec!["/home/u/a".to_string(), "/home/u/b/sub".to_string()]
        );
    }

    #[test]
    fn resolve_branches_dedupes_and_skips_unusable_repos() {
        let paths: Vec<String> = ["/wt1", "/wt2", "/main", "/detached", "/bitbucket", "/plain"]
            .map(String::from)
            .to_vec();
        let gitlab = "git@gitlab.com:g/storefront.git";
        let branches = resolve_branches(&paths, |path| match path {
            "/wt1" | "/wt2" => Some((gitlab.into(), "feature/a".into())),
            "/main" => Some((gitlab.into(), "main".into())),
            "/detached" => Some((gitlab.into(), "HEAD".into())),
            "/bitbucket" => Some(("git@bitbucket.org:acme/configs.git".into(), "main".into())),
            _ => None,
        });
        assert_eq!(
            branches,
            vec![branch(gitlab, "feature/a"), branch(gitlab, "main")]
        );
    }

    #[test]
    fn parse_glab_json_strips_draft_prefix() {
        let text = r#"[
            {"iid": 128, "title": "Draft: Add rate limiting", "web_url": "https://gitlab.com/g/r/-/merge_requests/128", "draft": true, "description": "long"},
            {"iid": 12, "title": "Fix login", "web_url": "https://gitlab.com/g/r/-/merge_requests/12", "draft": false},
            {"title": "missing iid", "web_url": "x"}
        ]"#;
        let mrs = parse_glab_json(text).unwrap();
        assert_eq!(
            mrs,
            vec![
                MrSummary {
                    number: 128,
                    title: "Add rate limiting".into(),
                    url: "https://gitlab.com/g/r/-/merge_requests/128".into(),
                    draft: true,
                },
                MrSummary {
                    number: 12,
                    title: "Fix login".into(),
                    url: "https://gitlab.com/g/r/-/merge_requests/12".into(),
                    draft: false,
                },
            ]
        );
    }

    #[test]
    fn parse_gh_json_reads_pr_fields() {
        let text = r#"[{"number": 4, "title": "Register hook", "url": "https://github.com/o/r/pull/4", "isDraft": true}]"#;
        assert_eq!(
            parse_gh_json(text).unwrap(),
            vec![MrSummary {
                number: 4,
                title: "Register hook".into(),
                url: "https://github.com/o/r/pull/4".into(),
                draft: true,
            }]
        );
    }

    #[test]
    fn parse_rejects_non_json_output() {
        assert!(parse_glab_json("not json").is_none());
        assert!(parse_gh_json("").is_none());
    }

    #[test]
    fn cache_freshness_follows_ttl() {
        let b = branch("git@gitlab.com:g/r.git", "feature/a");
        let mut cache = MrCache::default();
        assert!(!cache.is_fresh(&b, 1_000));
        cache.insert(&b, 1_000, vec![summary(1)]);
        assert!(cache.is_fresh(&b, 1_000 + MR_CACHE_TTL_SECS - 1));
        assert!(!cache.is_fresh(&b, 1_000 + MR_CACHE_TTL_SECS));
    }

    #[test]
    fn cache_round_trips_through_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("merge-requests.json");
        let b = branch("git@gitlab.com:g/r.git", "feature/a");
        let mut cache = MrCache::default();
        cache.insert(
            &b,
            42,
            vec![MrSummary {
                draft: true,
                ..summary(7)
            }],
        );
        cache.save(&path).unwrap();
        assert_eq!(MrCache::load(&path), cache);
    }

    #[test]
    fn cache_load_tolerates_missing_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("merge-requests.json");
        assert_eq!(MrCache::load(&path), MrCache::default());
        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(MrCache::load(&path), MrCache::default());
    }

    #[test]
    fn cache_prune_drops_old_entries() {
        let old = branch("git@gitlab.com:g/r.git", "old");
        let new = branch("git@gitlab.com:g/r.git", "new");
        let mut cache = MrCache::default();
        cache.insert(&old, 0, vec![]);
        cache.insert(&new, MR_CACHE_PRUNE_SECS, vec![]);
        cache.prune(MR_CACHE_PRUNE_SECS + 1);
        assert!(cache.get(&old).is_none());
        assert!(cache.get(&new).is_some());
    }

    #[test]
    fn merge_requests_follow_branch_order() {
        let a = branch("git@gitlab.com:g/api.git", "feature/a");
        let b = branch("git@github.com:o/web.git", "feature/b");
        let missing = branch("git@gitlab.com:g/api.git", "feature/none");
        let mut cache = MrCache::default();
        cache.insert(&b, 1, vec![summary(9)]);
        cache.insert(&a, 1, vec![summary(3), summary(5)]);
        let labels: Vec<String> = cache
            .merge_requests(&[a, b, missing])
            .iter()
            .map(|mr| format!("{} {} {}", mr.repo, mr.label(), mr.branch))
            .collect();
        assert_eq!(
            labels,
            vec!["api !3 feature/a", "api !5 feature/a", "web #9 feature/b"]
        );
    }

    #[test]
    fn fetch_all_keeps_input_order_across_chunks() {
        let branches: Vec<BranchRef> = (0..(MAX_PARALLEL_FETCHES as u64 * 2 + 1))
            .map(|i| branch("git@gitlab.com:g/r.git", &format!("b{i}")))
            .collect();
        let results = fetch_all(&branches, |b| {
            let n: u64 = b.branch[1..].parse().unwrap();
            n.is_multiple_of(2).then(|| vec![summary(n)])
        });
        assert_eq!(results.len(), branches.len());
        for (i, result) in results.iter().enumerate() {
            match result {
                Some(mrs) => assert_eq!(mrs[0].number, i as u64),
                None => assert!(!(i as u64).is_multiple_of(2)),
            }
        }
    }
}
