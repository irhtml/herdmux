use std::process::Command;

pub fn run_tmux(args: &[&str]) -> Option<String> {
    let output = Command::new("tmux").args(args).output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

/// Run `commands` in one tmux client invocation, joined with `;`, so a batch
/// costs one round trip instead of one per command. tmux stops at the first
/// failing command; returns whether every command ran, plus the stdout
/// produced up to that point.
pub fn run_tmux_batch(commands: &[Vec<&str>]) -> (bool, String) {
    // With no arguments tmux runs its default command, `new-session`.
    if commands.is_empty() {
        return (true, String::new());
    }
    match Command::new("tmux").args(batch_args(commands)).output() {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
        ),
        Err(_) => (false, String::new()),
    }
}

/// Join `commands` with standalone `;` arguments. No trailing `;`: tmux
/// reads it as an empty final command and runs `new-session`.
fn batch_args<'a>(commands: &[Vec<&'a str>]) -> Vec<&'a str> {
    commands.join(&";")
}

/// Run a tmux command, returning trimmed stdout on success and stderr on failure.
/// Used by the spawn/remove flow so the UI can surface a meaningful error message
/// instead of a silent fallthrough.
pub fn run_tmux_capture(args: &[&str]) -> Result<String, String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn tmux: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("tmux exited with status {}", output.status)
        } else {
            stderr
        })
    }
}

pub fn display_message(target: &str, format: &str) -> String {
    run_tmux(&["display-message", "-t", target, "-p", format])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Resolve the session name containing `pane_id`. Returns `None` when tmux
/// can't find the pane (e.g. it has just been closed).
pub fn pane_session_name(pane_id: &str) -> Option<String> {
    Some(display_message(pane_id, "#{session_name}")).filter(|s| !s.is_empty())
}

/// `-t` target for a new window in the session named `session`.
fn session_window_target(session: &str) -> String {
    format!("={session}:")
}

/// Resolve a split target in the originating window, never the sidebar itself.
pub fn worktree_split_target(origin: &str) -> Result<String, String> {
    let window = run_tmux_capture(&["display-message", "-t", origin, "-p", "#{window_id}"])?;
    if window.is_empty() {
        return Err("could not resolve originating tmux window".into());
    }
    let panes = run_tmux_capture(&[
        "list-panes",
        "-t",
        &window,
        "-F",
        "#{pane_id} #{pane_active} #{pane_last} #{@pane_role}",
    ])?;
    pick_worktree_split_target(origin, &panes)
        .ok_or_else(|| "could not find a non-sidebar pane in this window".into())
}

fn pick_worktree_split_target(origin: &str, panes: &str) -> Option<String> {
    panes
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pane = fields.next()?;
            let active = fields.next()? == "1";
            let last = fields.next()? == "1";
            let role = fields.next().unwrap_or_default();
            (role != "sidebar").then_some((pane, pane == origin, active, last))
        })
        .max_by_key(|(_, origin, active, last)| (*origin, *active, *last))
        .map(|(pane, _, _, _)| pane.to_string())
}

/// Split only the target pane, preserving the sidebar and existing window layout.
/// `detached` keeps focus on the current pane (`agent spawn` runs from
/// inside another agent and must not steal the user's cursor).
pub fn split_worktree_pane(target: &str, cwd: &str, detached: bool) -> Result<String, String> {
    let mut args = vec!["split-window", "-h"];
    if detached {
        args.push("-d");
    }
    args.extend(["-t", target, "-c", cwd, "-P", "-F", "#{pane_id}"]);
    run_tmux_capture(&args)
}

pub fn kill_pane(pane: &str) -> Result<(), String> {
    run_tmux_capture(&["kill-pane", "-t", pane]).map(|_| ())
}

pub fn set_spawn_pane_option(pane: &str, key: &str, value: &str) -> Result<(), String> {
    run_tmux_capture(&["set", "-p", "-t", pane, key, value]).map(|_| ())
}

/// Create a new tmux window in `session` whose initial cwd is `cwd` and whose
/// title is `name`. Returns `(pane_id, window_id)` on success — the window id
/// is used by the spawn flow to set markers at window scope so split panes
/// (e.g. Claude Code subagents) inherit them.
/// `detached` leaves the current window selected.
pub fn new_window(
    session: &str,
    cwd: &str,
    name: &str,
    detached: bool,
) -> Result<(String, String), String> {
    // `=name:` matches the session name exactly and asks for the next free
    // window index. A bare name would be read as a window index first when
    // it is a number, or as a prefix of a longer session name.
    let target = session_window_target(session);
    let mut args = vec!["new-window"];
    if detached {
        args.push("-d");
    }
    args.extend([
        "-t",
        &target,
        "-c",
        cwd,
        "-n",
        name,
        "-P",
        "-F",
        "#{pane_id} #{window_id}",
    ]);
    let out = run_tmux_capture(&args)?;
    let mut parts = out.split_whitespace();
    let pane = parts
        .next()
        .ok_or_else(|| "new-window returned no pane id".to_string())?
        .to_string();
    let window = parts
        .next()
        .ok_or_else(|| "new-window returned no window id".to_string())?
        .to_string();
    Ok((pane, window))
}

/// Set a user option at window scope. Needed so markers survive through
/// split panes that inherit from the window. Returns an error so the
/// spawn flow can roll back when a marker the remove path relies on
/// cannot be written — silently dropping the failure would leave an
/// un-removable pane.
pub fn set_window_option(window: &str, key: &str, value: &str) -> Result<(), String> {
    run_tmux_capture(&["set", "-w", "-t", window, key, value]).map(|_| ())
}

/// Send a command line to `target` (a pane id) and press Enter so the shell
/// executes it. Used to launch the agent binary right after window creation.
/// The text is sent with `-l` (literal) so nothing in `command` can collide
/// with tmux key names (e.g. `Tab`, `BSpace`); Enter is issued as a
/// separate invocation so it's interpreted as the Return key.
pub fn send_command(target: &str, command: &str) -> Result<(), String> {
    run_tmux_capture(&["send-keys", "-t", target, "-l", command])?;
    run_tmux_capture(&["send-keys", "-t", target, "Enter"]).map(|_| ())
}

/// Paste `text` into `target` as one bracketed paste, so multi-line
/// prompts land in the agent's input box instead of submitting at the
/// first newline. The text goes through `load-buffer` on stdin, never
/// argv, and the named buffer is deleted after pasting.
pub fn paste_text(target: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    let buffer = format!("tas-{}", std::process::id());
    let mut child = Command::new("tmux")
        .args(["load-buffer", "-b", &buffer, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn tmux: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("load-buffer: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("load-buffer: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    run_tmux_capture(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", target]).map(|_| ())
}

/// Send a single named key (e.g. `Enter`) to `target`.
pub fn send_key(target: &str, key: &str) -> Result<(), String> {
    run_tmux_capture(&["send-keys", "-t", target, key]).map(|_| ())
}

/// Leave copy mode (or any other pane mode) so pasted input reaches the
/// program instead of the mode's key table.
pub fn cancel_pane_mode(target: &str) -> Result<(), String> {
    run_tmux_capture(&["send-keys", "-t", target, "-X", "cancel"]).map(|_| ())
}

/// Last `lines` non-blank lines of the pane, joining wrapped lines.
/// Scrollback is included so a short screen still yields history.
pub fn capture_pane(target: &str, lines: usize) -> Result<String, String> {
    let start = format!("-{lines}");
    let raw = run_tmux_capture(&["capture-pane", "-p", "-J", "-t", target, "-S", &start])?;
    Ok(last_lines(&raw, lines))
}

fn last_lines(raw: &str, lines: usize) -> String {
    let all: Vec<&str> = raw.lines().map(str::trim_end).collect();
    let end = all.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
    all[end.saturating_sub(lines)..end].join("\n")
}

/// Kill the tmux window identified by `window_id` (e.g. `@7`).
pub fn kill_window(window_id: &str) -> Result<(), String> {
    run_tmux_capture(&["kill-window", "-t", window_id]).map(|_| ())
}

/// Resolve the client that should follow a jump: the most recently active
/// client attached to the session containing `own_pane_id` (the sidebar's own
/// pane). The keypress that triggered the jump bumps that client's activity,
/// so it identifies the terminal the user is typing in. Without an explicit
/// `-c`, `switch-client` falls back to tmux's guess of the current client,
/// which can yank a client attached to a *different* session onto the target.
pub fn jump_client(own_pane_id: &str) -> Option<String> {
    let session_id = display_message(own_pane_id, "#{session_id}");
    if session_id.is_empty() {
        return None;
    }
    let output = run_tmux(&[
        "list-clients",
        "-t",
        &session_id,
        "-F",
        "#{client_activity} #{client_name}",
    ])?;
    most_recent_client(&output)
}

fn most_recent_client(list_clients_output: &str) -> Option<String> {
    list_clients_output
        .lines()
        .filter_map(|line| {
            let (activity, name) = line.trim().split_once(' ')?;
            Some((activity.parse::<u64>().ok()?, name.to_string()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, name)| name)
}

/// A tmux client: its name (the tty) and the pid of its `tmux` process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: String,
    pub pid: u32,
}

/// The client that should show `pane_id` when the jump comes from outside
/// tmux, such as a notification click: the most recently active client on
/// the pane's session, or else the most recently active client of all.
/// `None` when the pane is gone or no client is attached.
pub fn client_for_pane(pane_id: &str) -> Option<ClientInfo> {
    let session_id = display_message(pane_id, "#{session_id}");
    if session_id.is_empty() {
        return None;
    }
    const FORMAT: &str = "#{client_activity} #{client_pid} #{client_name}";
    run_tmux(&["list-clients", "-t", &session_id, "-F", FORMAT])
        .as_deref()
        .and_then(most_recent_client_info)
        .or_else(|| {
            run_tmux(&["list-clients", "-F", FORMAT])
                .as_deref()
                .and_then(most_recent_client_info)
        })
}

/// Parse `#{client_activity} #{client_pid} #{client_name}` lines.
fn most_recent_client_info(list_clients_output: &str) -> Option<ClientInfo> {
    list_clients_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim().splitn(3, ' ');
            let activity = fields.next()?.parse::<u64>().ok()?;
            let pid = fields.next()?.parse::<u32>().ok()?;
            let name = fields.next().filter(|n| !n.is_empty())?;
            Some((
                activity,
                ClientInfo {
                    name: name.into(),
                    pid,
                },
            ))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, client)| client)
}

/// Jump from the sidebar in `own_pane_id` to `pane_id`.
pub fn select_pane(pane_id: &str, own_pane_id: &str) {
    show_pane(pane_id, jump_client(own_pane_id).as_deref());
}

/// Bring `pane_id` on screen: select the pane and its window, then switch
/// `client` (or tmux's guess of the current client) to its session.
pub fn show_pane(pane_id: &str, client: Option<&str>) {
    let ids = display_message(pane_id, "#{session_id} #{window_id}");
    let mut ids = ids.split_whitespace();
    let (Some(session_id), Some(window_id)) = (ids.next(), ids.next()) else {
        return;
    };
    let _ = run_tmux_batch(&show_pane_commands(pane_id, window_id, session_id, client));
}

/// Pane and window first, client last, in one tmux call: the client lands
/// on the pane directly, so the unread mark of a window it would otherwise
/// pass through on the way stays (see `mark-read`).
fn show_pane_commands<'a>(
    pane_id: &'a str,
    window_id: &'a str,
    session_id: &'a str,
    client: Option<&'a str>,
) -> Vec<Vec<&'a str>> {
    let mut switch = vec!["switch-client"];
    if let Some(client) = client {
        switch.extend(["-c", client]);
    }
    switch.extend(["-t", session_id]);
    vec![
        vec!["select-pane", "-t", pane_id],
        vec!["select-window", "-t", window_id],
        switch,
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        ClientInfo, batch_args, last_lines, most_recent_client, most_recent_client_info,
        pick_worktree_split_target, session_window_target, show_pane_commands,
    };

    #[test]
    fn new_windows_target_the_session_by_exact_name() {
        assert_eq!(session_window_target("7"), "=7:");
        assert_eq!(session_window_target("main"), "=main:");
    }

    #[test]
    fn show_pane_selects_pane_and_window_before_switching_the_client() {
        assert_eq!(
            show_pane_commands("%4", "@2", "$1", Some("/dev/pts/3")),
            vec![
                vec!["select-pane", "-t", "%4"],
                vec!["select-window", "-t", "@2"],
                vec!["switch-client", "-c", "/dev/pts/3", "-t", "$1"],
            ]
        );
        assert_eq!(
            show_pane_commands("%4", "@2", "$1", None)[2],
            vec!["switch-client", "-t", "$1"]
        );
    }

    #[test]
    fn most_recent_client_info_picks_the_latest_activity() {
        let output = "100 4321 /dev/pts/0\n300 999 /dev/pts/62\n200 77 /dev/pts/3\nbad line\n";
        assert_eq!(
            most_recent_client_info(output),
            Some(ClientInfo {
                name: "/dev/pts/62".into(),
                pid: 999,
            })
        );
        assert_eq!(most_recent_client_info(""), None);
    }

    #[test]
    fn batch_args_separates_commands_without_trailing_separator() {
        let commands = vec![vec!["kill-pane", "-t", "%1"], vec!["kill-pane", "-t", "%2"]];
        assert_eq!(
            batch_args(&commands),
            vec!["kill-pane", "-t", "%1", ";", "kill-pane", "-t", "%2"]
        );
        assert_eq!(batch_args(&[vec!["list-panes"]]), vec!["list-panes"]);
    }

    #[test]
    fn last_lines_drops_trailing_blank_rows() {
        let raw = "a\nb  \nc\n\n   \n";
        assert_eq!(last_lines(raw, 2), "b\nc");
        assert_eq!(last_lines(raw, 10), "a\nb\nc");
        assert_eq!(last_lines("\n\n", 3), "");
    }

    #[test]
    fn worktree_split_preserves_sidebar_and_prefers_last_main_pane() {
        let panes = "%0 1 0 sidebar\n%1 0 0\n%2 0 1\n";
        assert_eq!(
            pick_worktree_split_target("%0", panes).as_deref(),
            Some("%2")
        );
    }

    #[test]
    fn worktree_split_cli_prefers_origin_over_other_active_panes() {
        let panes = "%0 0 0\n%1 1 1\n%2 0 0 sidebar\n";
        assert_eq!(
            pick_worktree_split_target("%0", panes).as_deref(),
            Some("%0")
        );
    }

    #[test]
    fn worktree_split_uses_active_main_pane_or_fallback() {
        assert_eq!(
            pick_worktree_split_target("%0", "%0 0 0 sidebar\n%1 1 0\n%2 0 1").as_deref(),
            Some("%1")
        );
        assert_eq!(
            pick_worktree_split_target("%0", "%0 1 0 sidebar\n%1 0 0").as_deref(),
            Some("%1")
        );
        assert!(pick_worktree_split_target("%0", "%0 1 0 sidebar\nbad line").is_none());
    }

    #[test]
    fn most_recent_client_picks_highest_activity() {
        let output = "1787899131 /dev/pts/0\n1787899155 /dev/pts/107\n";
        assert_eq!(most_recent_client(output), Some("/dev/pts/107".to_string()));
    }

    #[test]
    fn most_recent_client_skips_malformed_lines() {
        let output = "garbage\n1787899131 /dev/pts/0\nnot-a-number /dev/pts/9\n";
        assert_eq!(most_recent_client(output), Some("/dev/pts/0".to_string()));
    }

    #[test]
    fn most_recent_client_empty_output_is_none() {
        assert_eq!(most_recent_client(""), None);
    }
}
