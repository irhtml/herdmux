//! Click a desktop notification to jump to its agent (Linux).
//!
//! The hook must not wait for a click, so it starts `herdmux notify-wait`
//! in its own session and returns. That helper shows the notification with
//! an "Open" action and waits for `notify-send` to report a click. On a
//! click it moves a tmux client to the agent's pane and focuses the
//! terminal window that client runs in.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::tmux;

/// Hidden CLI subcommand that runs [`cmd_notify_wait`].
pub const WAIT_SUBCOMMAND: &str = "notify-wait";
/// Name of the action `notify-send` prints when the notification is
/// clicked. Daemons run the `default` action on a plain click.
const ACTION: &str = "default";
/// Notifications that never expire still free their helper after this.
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
/// A `notify-send` that fails this fast did not show anything (for example
/// an old version without `--action`), so a plain notification follows.
const FAST_FAILURE: Duration = Duration::from_secs(2);
/// How far up the process tree to look for the terminal window: tmux
/// client, shell, terminal, plus room for wrappers.
const MAX_ANCESTORS: usize = 8;

/// Start the helper for one notification. An error means it did not start,
/// and the caller sends a plain notification instead.
pub(super) fn spawn_waiter(pane_id: &str, title: &str, body: &str) -> std::io::Result<()> {
    // Its own session, so it outlives the hook, and so its own process
    // group: a newer notification can stop it together with its notify-send.
    crate::process::spawn_self_detached(&[WAIT_SUBCOMMAND, pane_id, title, body])
}

/// `herdmux notify-wait <pane> <title> <body>`.
pub fn cmd_notify_wait(args: &[String]) -> i32 {
    let [pane_id, title, body] = args else {
        eprintln!("usage: herdmux {WAIT_SUBCOMMAND} <pane> <title> <body>");
        return 2;
    };
    stop_previous_waiter(pane_id);
    tmux::set_pane_option(
        pane_id,
        tmux::PANE_NOTIFY_PID,
        &std::process::id().to_string(),
    );
    match show_and_wait(pane_id, title, body) {
        Ok(true) => jump_to(pane_id),
        Ok(false) => {}
        Err(_) => {
            let _ = Command::new("notify-send")
                .args(super::plain_notify_send_args(title, body))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    0
}

fn action_notify_send_args(pane_id: &str, title: &str, body: &str) -> Vec<String> {
    vec![
        "--app-name=herdmux".into(),
        "--urgency=normal".into(),
        format!("--action={ACTION}=Open"),
        // dunst: a newer notification for the same agent replaces this one.
        format!("--hint=string:x-dunst-stack-tag:herdmux-{pane_id}"),
        "--".into(),
        title.into(),
        body.into(),
    ]
}

/// Show the notification and wait. `Ok(true)` when it was clicked,
/// `Ok(false)` when it closed or timed out, `Err` when `notify-send`
/// failed at once.
fn show_and_wait(pane_id: &str, title: &str, body: &str) -> Result<bool, ()> {
    let mut child = Command::new("notify-send")
        .args(action_notify_send_args(pane_id, title, body))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() >= MAX_WAIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(false);
            }
            Ok(None) => sleep(Duration::from_millis(100)),
            Err(_) => return Err(()),
        }
    };
    if !status.success() {
        return if start.elapsed() < FAST_FAILURE {
            Err(())
        } else {
            Ok(false)
        };
    }
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    Ok(was_clicked(&output))
}

fn was_clicked(notify_send_output: &str) -> bool {
    notify_send_output.lines().any(|line| line.trim() == ACTION)
}

/// Stop the helper still waiting on this pane's previous notification, so
/// helpers do not pile up and an old notification cannot jump later.
fn stop_previous_waiter(pane_id: &str) {
    let raw = tmux::get_pane_option_value(pane_id, tmux::PANE_NOTIFY_PID);
    let Ok(pid) = raw.trim().parse::<i32>() else {
        return;
    };
    if pid <= 1 || pid == std::process::id() as i32 {
        return;
    }
    // The id may have been reused by an unrelated process since.
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return;
    };
    if is_waiter_for(&cmdline, pane_id) {
        // The helper leads its own process group (setsid), so this stops
        // its notify-send too.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
}

/// Whether a `/proc/<pid>/cmdline` belongs to a helper for `pane_id`.
fn is_waiter_for(cmdline: &[u8], pane_id: &str) -> bool {
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
    args.get(1) == Some(&WAIT_SUBCOMMAND.as_bytes()) && args.get(2) == Some(&pane_id.as_bytes())
}

fn jump_to(pane_id: &str) {
    let Some(client) = tmux::client_for_pane(pane_id) else {
        return;
    };
    tmux::show_pane(pane_id, Some(&client.name));
    focus_terminal(client.pid);
}

/// Raise the X11 window of the terminal running the tmux client `pid`.
fn focus_terminal(client_pid: u32) {
    if std::env::var_os("DISPLAY").is_none() {
        return;
    }
    let Some(window) = ancestors(client_pid).find_map(window_of_pid) else {
        return;
    };
    // i3 moves to the window's workspace; a generic activate request is
    // often only marked urgent there.
    let focused_by_i3 = Command::new("i3-msg")
        .arg(format!("[id={window}] focus"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !focused_by_i3 {
        let _ = Command::new("xdotool")
            .args(["windowactivate", &window])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `pid`, its parent, its grandparent, and so on.
fn ancestors(pid: u32) -> impl Iterator<Item = u32> {
    std::iter::successors(Some(pid), |&pid| {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parent_pid(&stat).filter(|&ppid| ppid > 1)
    })
    .take(MAX_ANCESTORS)
}

/// The parent pid from a `/proc/<pid>/stat` line. The command name in
/// parentheses may itself hold spaces and parentheses, so parse after the
/// last `)`.
fn parent_pid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

fn window_of_pid(pid: u32) -> Option<String> {
    let output = Command::new("xdotool")
        .args(["search", "--pid", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_args_carry_the_action_and_a_per_pane_stack_tag() {
        assert_eq!(
            action_notify_send_args("%12", "repo / claude", "- done"),
            [
                "--app-name=herdmux",
                "--urgency=normal",
                "--action=default=Open",
                "--hint=string:x-dunst-stack-tag:herdmux-%12",
                "--",
                "repo / claude",
                "- done",
            ]
        );
    }

    #[test]
    fn only_the_default_action_counts_as_a_click() {
        assert!(was_clicked("default\n"));
        assert!(!was_clicked(""));
        assert!(!was_clicked("1\n"));
    }

    #[test]
    fn waiter_is_recognized_by_subcommand_and_pane() {
        let cmdline = b"/usr/bin/herdmux\0notify-wait\0%12\0title\0body\0";
        assert!(is_waiter_for(cmdline, "%12"));
        assert!(!is_waiter_for(cmdline, "%1"));
        assert!(!is_waiter_for(b"/usr/bin/vim\0notify-wait\0", "%12"));
        assert!(!is_waiter_for(b"", "%12"));
    }

    #[test]
    fn parent_pid_survives_odd_command_names() {
        assert_eq!(parent_pid("4321 (tmux: client) S 77 4321 4321 0"), Some(77));
        assert_eq!(parent_pid("9 (a) b) R 1 9 9"), Some(1));
        assert_eq!(parent_pid("garbage"), None);
    }
}
