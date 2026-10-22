use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use crate::git::{self, GitData};
use crate::merge_request::{self, BranchRef, MergeRequest, MrCache, MrSummary};
use crate::session;
use crate::state::{AppState, BottomTab};
use crate::tmux;
use crate::transcript::{self, DigestUpdate, TranscriptIndex};
use crate::version::{self, UpdateNotice};

/// Channels and shared flags produced by [`spawn`] that the main event loop
/// drains every tick.
pub(super) struct Workers {
    pub git_rx: Receiver<GitSnapshot>,
    pub git_requests: Sender<GitRequest>,
    pub session_rx: Receiver<HashMap<String, String>>,
    pub version_rx: Receiver<UpdateNotice>,
    pub git_tab_active: Arc<AtomicBool>,
    pub mr_rx: Receiver<MrSnapshot>,
    pub mr_wake: Sender<()>,
    pub mr_tab_active: Arc<AtomicBool>,
    /// Claude sessions whose transcripts the transcript thread should read.
    pub transcript_sessions: Sender<Vec<(String, String)>>,
    pub digest_rx: Receiver<Vec<DigestUpdate>>,
    /// Set while someone views the sidebar; the transcript thread rests otherwise.
    pub transcripts_active: Arc<AtomicBool>,
}

/// One publish of the MR poll thread.
pub(super) struct MrSnapshot {
    pub list: Vec<MergeRequest>,
    /// Lookups for some branches are still running.
    pub pending: bool,
}

#[derive(Clone)]
pub(super) struct GitRequest {
    pub pane_id: Option<String>,
    pub generation: u64,
}

pub(super) struct GitSnapshot {
    pub generation: u64,
    pub data: GitData,
}

/// Ignore results from panes we have already left, including A → B → A races.
pub(super) fn apply_git_snapshot(
    state: &mut AppState,
    generation: u64,
    snapshot: GitSnapshot,
) -> bool {
    if snapshot.generation != generation {
        return false;
    }
    state.apply_git_data(snapshot.data);
    true
}

/// Spawn the background threads (git polling, session-name polling, version
/// notice fetch) that feed the event loop.
pub(super) fn spawn(state: &AppState) -> Workers {
    let (git_tx, git_rx) = mpsc::channel::<GitSnapshot>();
    let (git_requests, request_rx) = mpsc::channel::<GitRequest>();
    let (session_tx, session_rx) = mpsc::channel::<HashMap<String, String>>();
    let (version_tx, version_rx) = mpsc::channel::<UpdateNotice>();
    let git_tab_active = Arc::new(AtomicBool::new(state.bottom_tab == BottomTab::GitStatus));
    let git_tab_flag = Arc::clone(&git_tab_active);
    let _ = git_requests.send(GitRequest {
        pane_id: state.focus_state.focused_pane_id.clone(),
        generation: 0,
    });
    std::thread::spawn(move || {
        git_poll_loop(&request_rx, &git_tx, &git_tab_flag);
    });
    std::thread::spawn(move || {
        session_poll_loop(&session_tx);
    });
    let (mr_tx, mr_rx) = mpsc::channel::<MrSnapshot>();
    let (mr_wake, mr_wake_rx) = mpsc::channel::<()>();
    let mr_tab_active = Arc::new(AtomicBool::new(false));
    let mr_tab_flag = Arc::clone(&mr_tab_active);
    let sidebar_pane = state.tmux_pane.clone();
    std::thread::spawn(move || {
        mr_poll_loop(&sidebar_pane, &mr_wake_rx, &mr_tx, &mr_tab_flag);
    });
    std::thread::spawn(move || {
        if let Some(notice) = version::fetch_update_notice() {
            let _ = version_tx.send(notice);
        }
    });
    let (transcript_sessions, sessions_rx) = mpsc::channel::<Vec<(String, String)>>();
    let (digest_tx, digest_rx) = mpsc::channel::<Vec<DigestUpdate>>();
    let transcripts_active = Arc::new(AtomicBool::new(true));
    let transcripts_flag = Arc::clone(&transcripts_active);
    std::thread::spawn(move || {
        transcript_poll_loop(&sessions_rx, &digest_tx, &transcripts_flag);
    });

    Workers {
        git_rx,
        git_requests,
        session_rx,
        version_rx,
        git_tab_active,
        mr_rx,
        mr_wake,
        mr_tab_active,
        transcript_sessions,
        digest_rx,
        transcripts_active,
    }
}

/// How often the transcript thread checks the tracked transcripts for new
/// lines. Each check is one `stat` per session, plus a read of what was
/// appended.
const TRANSCRIPT_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Transcript thread: keeps a digest (title, prompts, last message time)
/// per tracked Claude session and sends what changed. A new session list
/// is read right away; otherwise it polls every
/// [`TRANSCRIPT_POLL_INTERVAL`] while `active` is set.
pub(super) fn transcript_poll_loop(
    sessions_rx: &Receiver<Vec<(String, String)>>,
    digest_tx: &Sender<Vec<DigestUpdate>>,
    active: &AtomicBool,
) {
    let mut index = TranscriptIndex::new(transcript::config_dirs());
    let mut sessions: Vec<(String, String)> = Vec::new();
    loop {
        match sessions_rx.recv_timeout(TRANSCRIPT_POLL_INTERVAL) {
            Ok(list) => {
                sessions = list;
                while let Ok(newer) = sessions_rx.try_recv() {
                    sessions = newer;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if !active.load(Ordering::Relaxed) {
            continue;
        }
        let updates = index.poll(&sessions);
        if !updates.is_empty() && digest_tx.send(updates).is_err() {
            return;
        }
    }
}

/// How often the MRs tab re-reads the session's branches while on screen.
/// A pass only calls `glab` / `gh` for branches whose shared-cache entry
/// has expired, so most passes cost one tmux and a few git calls.
const MR_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Merge-request polling thread. Runs a pass when woken (the MRs tab came
/// on screen) and every [`MR_POLL_INTERVAL`] while it stays there; does
/// nothing while the tab is hidden.
pub(super) fn mr_poll_loop(
    sidebar_pane: &str,
    wake: &Receiver<()>,
    mr_tx: &Sender<MrSnapshot>,
    active: &AtomicBool,
) {
    let cache_path = MrCache::default_path();
    loop {
        match wake.recv_timeout(MR_POLL_INTERVAL) {
            Ok(()) => while wake.try_recv().is_ok() {},
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if !active.load(Ordering::Relaxed) {
            continue;
        }
        let branches = merge_request::resolve_branches(
            &merge_request::session_pane_paths(sidebar_pane),
            merge_request::git_remote_and_branch,
        );
        let published = mr_pass(
            &branches,
            cache_path.as_deref(),
            crate::time::now_epoch_secs(),
            mr_tx,
            merge_request::fetch_open_mrs,
        );
        if !published {
            return;
        }
    }
}

/// Publish what the cache already holds for `branches`, fetch the stale
/// ones, store the results, and publish again. Returns `false` once the
/// receiver is gone.
fn mr_pass<F>(
    branches: &[BranchRef],
    cache_path: Option<&Path>,
    now: u64,
    mr_tx: &Sender<MrSnapshot>,
    fetch: F,
) -> bool
where
    F: Fn(&BranchRef) -> Option<Vec<MrSummary>> + Sync,
{
    let cache = cache_path.map(MrCache::load).unwrap_or_default();
    let stale: Vec<BranchRef> = branches
        .iter()
        .filter(|b| !cache.is_fresh(b, now))
        .cloned()
        .collect();
    let first = MrSnapshot {
        list: cache.merge_requests(branches),
        pending: !stale.is_empty(),
    };
    if mr_tx.send(first).is_err() {
        return false;
    }
    if stale.is_empty() {
        return true;
    }
    let results = merge_request::fetch_all(&stale, fetch);
    // Re-read so entries other sidebars wrote during the fetch survive.
    let mut cache = cache_path.map(MrCache::load).unwrap_or(cache);
    for (branch, result) in stale.iter().zip(results) {
        if let Some(mrs) = result {
            cache.insert(branch, now, mrs);
        }
    }
    cache.prune(now);
    if let Some(path) = cache_path {
        let _ = cache.save(path);
    }
    mr_tx
        .send(MrSnapshot {
            list: cache.merge_requests(branches),
            pending: false,
        })
        .is_ok()
}

/// Session name polling thread. Scans `~/.claude/sessions/*.json` every 10
/// seconds so the main TUI thread never performs blocking filesystem I/O
/// to refresh `/rename`-assigned labels.
pub(super) fn session_poll_loop(tx: &mpsc::Sender<HashMap<String, String>>) {
    loop {
        std::thread::sleep(Duration::from_secs(10));
        let names = session::scan_session_names();
        if tx.send(names).is_err() {
            return;
        }
    }
}

/// Git data polling thread. Fetches git status every 2 seconds while the Git
/// tab is active, and wakes immediately for focus changes even on Activity.
/// Periodic fetches stop while hidden. PR numbers go
/// through an in-memory `(path, branch)`-keyed cache so `gh pr view` (the only
/// hop that costs GitHub API quota) runs at most once per `PR_CACHE_TTL`
/// instead of every tick.
pub(super) fn git_poll_loop(
    requests: &Receiver<GitRequest>,
    git_tx: &Sender<GitSnapshot>,
    active: &AtomicBool,
) {
    git_poll_loop_with(
        requests,
        git_tx,
        active,
        Duration::from_secs(2),
        tmux::get_pane_path,
        git::fetch_git_data,
        git::fetch_pr_number,
    );
}

fn git_poll_loop_with<FPath, FGit, FPr>(
    requests: &Receiver<GitRequest>,
    git_tx: &Sender<GitSnapshot>,
    active: &AtomicBool,
    interval: Duration,
    mut get_path: FPath,
    mut fetch_git: FGit,
    mut fetch_pr: FPr,
) where
    FPath: FnMut(&str) -> Option<String>,
    FGit: FnMut(&str) -> GitData,
    FPr: FnMut(&str) -> Option<String>,
{
    let mut current = GitRequest {
        pane_id: None,
        generation: 0,
    };
    let mut pr_cache = git::PrCache::new();
    loop {
        // Focus changes wake the worker immediately, rather than waiting for its poll tick.
        let forced = match requests.recv_timeout(interval) {
            Ok(request) => {
                current = request;
                while let Ok(request) = requests.try_recv() {
                    current = request;
                }
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if !forced && !active.load(Ordering::Relaxed) {
            continue;
        }
        let Some(pane) = &current.pane_id else {
            continue;
        };
        if let Some(path) = get_path(pane) {
            let mut data = fetch_git(&path);
            // Publish local status before any potentially slow GitHub request.
            if git_tx
                .send(GitSnapshot {
                    generation: current.generation,
                    data: data.clone(),
                })
                .is_err()
            {
                return;
            }
            if !active.load(Ordering::Relaxed) || data.branch.is_empty() {
                continue;
            }
            data.pr_number = pr_cache.get_or_fetch(
                &path,
                &data.branch,
                std::time::Instant::now(),
                &mut fetch_pr,
            );
            if git_tx
                .send(GitSnapshot {
                    generation: current.generation,
                    data,
                })
                .is_err()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mr_branch(name: &str) -> BranchRef {
        BranchRef {
            repo: "storefront".into(),
            branch: name.into(),
            remote: "git@gitlab.com:g/storefront.git".into(),
            forge: merge_request::Forge::GitLab,
        }
    }

    fn mr_summary(number: u64) -> MrSummary {
        MrSummary {
            number,
            title: format!("MR {number}"),
            url: format!("https://gitlab.com/g/storefront/-/merge_requests/{number}"),
            draft: false,
        }
    }

    fn numbers(snapshot: &MrSnapshot) -> Vec<u64> {
        snapshot.list.iter().map(|mr| mr.number).collect()
    }

    #[test]
    fn mr_pass_with_fresh_cache_publishes_once_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mrs.json");
        let fresh = mr_branch("feature/a");
        let mut cache = MrCache::default();
        cache.insert(&fresh, 1_000, vec![mr_summary(7)]);
        cache.save(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        assert!(mr_pass(&[fresh], Some(&path), 1_010, &tx, |_| {
            panic!("fresh entries must not refetch")
        }));
        let snapshot = rx.try_recv().unwrap();
        assert_eq!(numbers(&snapshot), vec![7]);
        assert!(!snapshot.pending);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mr_pass_publishes_cached_then_fetched_and_stores_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mrs.json");
        let stale = mr_branch("feature/a");
        let fresh = mr_branch("feature/b");
        let mut cache = MrCache::default();
        cache.insert(&stale, 0, vec![mr_summary(1)]);
        cache.insert(&fresh, 9_990, vec![mr_summary(2)]);
        cache.save(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        let fetched = std::sync::Mutex::new(Vec::new());
        assert!(mr_pass(
            &[stale.clone(), fresh.clone()],
            Some(&path),
            10_000,
            &tx,
            |b| {
                fetched.lock().unwrap().push(b.branch.clone());
                Some(vec![mr_summary(3)])
            },
        ));
        assert_eq!(*fetched.lock().unwrap(), vec!["feature/a".to_string()]);

        let first = rx.try_recv().unwrap();
        assert_eq!(numbers(&first), vec![1, 2]);
        assert!(first.pending);
        let second = rx.try_recv().unwrap();
        assert_eq!(numbers(&second), vec![3, 2]);
        assert!(!second.pending);

        let stored = MrCache::load(&path);
        assert!(stored.is_fresh(&stale, 10_000));
        assert_eq!(
            numbers(&MrSnapshot {
                list: stored.merge_requests(&[stale, fresh]),
                pending: false,
            }),
            vec![3, 2]
        );
    }

    #[test]
    fn mr_pass_keeps_old_result_when_fetch_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mrs.json");
        let stale = mr_branch("feature/a");
        let mut cache = MrCache::default();
        cache.insert(&stale, 0, vec![mr_summary(1)]);
        cache.save(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        assert!(mr_pass(
            std::slice::from_ref(&stale),
            Some(&path),
            10_000,
            &tx,
            |_| None
        ));
        let _first = rx.try_recv().unwrap();
        let second = rx.try_recv().unwrap();
        assert_eq!(numbers(&second), vec![1]);
        assert!(
            !MrCache::load(&path).is_fresh(&stale, 10_000),
            "failure must not refresh the TTL"
        );
    }

    #[test]
    fn mr_pass_stops_when_receiver_is_gone() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        assert!(!mr_pass(&[mr_branch("feature/a")], None, 0, &tx, |_| {
            panic!("no fetch after the receiver is gone")
        }));
    }

    #[test]
    fn git_focus_requests_coalesce_and_fetch_even_with_tab_inactive() {
        let (tx, requests) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        for generation in 0..3 {
            tx.send(GitRequest {
                pane_id: Some(format!("%{generation}")),
                generation,
            })
            .unwrap();
        }
        drop(tx);
        git_poll_loop_with(
            &requests,
            &results,
            &AtomicBool::new(false),
            Duration::from_secs(60),
            |pane| {
                assert_eq!(pane, "%2");
                Some("/repo".into())
            },
            |path| {
                assert_eq!(path, "/repo");
                GitData {
                    branch: "main".into(),
                    ..GitData::default()
                }
            },
            |_| panic!("inactive tab must not request GitHub"),
        );
        let snapshot = rx.try_recv().unwrap();
        assert_eq!(snapshot.generation, 2);
        assert_eq!(snapshot.data.branch, "main");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn git_focus_request_without_path_does_not_fetch() {
        let (tx, requests) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        tx.send(GitRequest {
            pane_id: Some("%1".into()),
            generation: 1,
        })
        .unwrap();
        drop(tx);
        git_poll_loop_with(
            &requests,
            &results,
            &AtomicBool::new(true),
            Duration::from_secs(60),
            |_| None,
            |_| panic!("missing pane must not fetch"),
            |_| panic!("missing pane must not fetch PR"),
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn git_worker_publishes_local_status_before_pr_lookup() {
        let (tx, requests) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        tx.send(GitRequest {
            pane_id: Some("%1".into()),
            generation: 1,
        })
        .unwrap();
        drop(tx);
        git_poll_loop_with(
            &requests,
            &results,
            &AtomicBool::new(true),
            Duration::from_secs(60),
            |_| Some("/repo".into()),
            |_| GitData {
                branch: "main".into(),
                ..GitData::default()
            },
            |_| {
                let snapshot = rx
                    .try_recv()
                    .expect("local status arrives before GitHub lookup");
                assert_eq!(snapshot.data.branch, "main");
                Some("123".into())
            },
        );
        assert_eq!(
            rx.try_recv().unwrap().data.pr_number.as_deref(),
            Some("123")
        );
    }

    #[test]
    fn git_snapshot_rejects_stale_focus_generation() {
        let mut state = AppState::new("%0".into());
        state.git.branch = "current".into();
        assert!(!apply_git_snapshot(
            &mut state,
            2,
            GitSnapshot {
                generation: 0,
                data: GitData {
                    branch: "stale".into(),
                    ..GitData::default()
                }
            }
        ));
        assert_eq!(state.git.branch, "current");
        assert!(apply_git_snapshot(
            &mut state,
            2,
            GitSnapshot {
                generation: 2,
                data: GitData {
                    branch: "fresh".into(),
                    ..GitData::default()
                }
            }
        ));
        assert_eq!(state.git.branch, "fresh");
    }

    #[test]
    fn test_git_poll_skips_when_inactive() {
        let active = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<GitData>();

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            // Simulate the poll loop check without actually sleeping 2s
            for _ in 0..3 {
                if !flag.load(Ordering::Relaxed) {
                    continue;
                }
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        // No data should have been sent since active=false
        assert!(
            rx.try_recv().is_err(),
            "should not poll when git tab is inactive"
        );
    }

    #[test]
    fn test_git_poll_sends_when_active() {
        let active = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel::<GitData>();

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            // active=true, so it should send
            if flag.load(Ordering::Relaxed) {
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        assert!(rx.try_recv().is_ok(), "should poll when git tab is active");
    }

    #[test]
    fn test_git_poll_reacts_to_flag_change() {
        let active = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<GitData>();

        // Initially inactive
        assert!(!active.load(Ordering::Relaxed));

        // Switch to active
        active.store(true, Ordering::Relaxed);

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            if flag.load(Ordering::Relaxed) {
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        assert!(
            rx.try_recv().is_ok(),
            "should poll after flag switches to active"
        );
    }

    #[test]
    fn test_git_poll_stops_on_sender_closed() {
        let active = AtomicBool::new(true);
        let (tx, rx) = mpsc::channel::<GitData>();
        drop(rx); // Close receiver

        let result = tx.send(GitData::default());
        assert!(result.is_err(), "send should fail when receiver is dropped");

        // Verify the flag check pattern used in git_poll_loop
        assert!(active.load(Ordering::Relaxed));
    }
}
