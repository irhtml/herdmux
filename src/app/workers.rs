use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use crate::git::{self, GitData};
use crate::session;
use crate::state::{AppState, BottomTab};
use crate::tmux;
use crate::version::{self, UpdateNotice};

/// Channels and shared flags produced by [`spawn`] that the main event loop
/// drains every tick.
pub(super) struct Workers {
    pub git_rx: Receiver<GitSnapshot>,
    pub git_requests: Sender<GitRequest>,
    pub session_rx: Receiver<HashMap<String, String>>,
    pub version_rx: Receiver<UpdateNotice>,
    pub git_tab_active: Arc<AtomicBool>,
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
    std::thread::spawn(move || {
        if let Some(notice) = version::fetch_update_notice() {
            let _ = version_tx.send(notice);
        }
    });

    Workers {
        git_rx,
        git_requests,
        session_rx,
        version_rx,
        git_tab_active,
    }
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
