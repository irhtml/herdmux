//! `herdmux park`: close an agent pane without losing its session.
//!
//! Parking asks the agent for a short handoff, saves a record of the
//! session (see `crate::park`), ends the agent with its exit command and
//! closes the pane. `park resume` opens the session again in a new window.

use std::path::Path;
use std::time::Duration;

use super::env::{AgentTmux, PaneState, RealTmux};
use super::prompt::{Submit, check_promptable, submit};
use super::wait::{Outcome, Until, agent_known, wait_until};
use super::{EXIT_ERROR, EXIT_OK, EXIT_REFUSED, EXIT_USAGE, self_pane, target};
use crate::activity::AgentResponse;
use crate::cli::args::{Args, Spec};
use crate::park::{self, HANDOFF_PROMPT, Parked};
use crate::resume::ResumeCommand;
use crate::tmux::{self, PaneLocation};

const HANDOFF_TIMEOUT_MS: u64 = 120_000;
/// After an Escape, how long the agent gets to settle before it is exited.
const SETTLE_TIMEOUT_MS: u64 = 10_000;
const EXIT_TIMEOUT_MS: u64 = 10_000;
/// How long a resumed agent gets to report its session before the parked
/// record is kept as a fallback.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(300);

const USAGE: &str = "\
usage: herdmux park <target> [--no-handoff] [--client C]
       herdmux park resume <session_id> [--client C]
       herdmux park forget <session_id>

Close an agent pane and keep its session, to resume it later from the
finder (prefix + a) or with `park resume`.

  park       Ask the agent for a short handoff (goal, state, next step),
             save the session, exit the agent and close the pane.
             --no-handoff skips the question and keeps its last reply.
  resume     Open a parked session in a new window and forget the record.
  forget     Forget a parked session.

--client names the tmux client that shows the result and the new window.
targets: pane id (%12), session:window.pane, a @pane_desc tag, or a worktree name.";

pub(crate) fn cmd_park(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            if args.is_empty() { EXIT_USAGE } else { EXIT_OK }
        }
        Some("resume") => run_resume(&args[1..]),
        Some("forget") => run_forget(&args[1..]),
        Some(_) => run_park(args),
    }
}

fn usage_error(message: &str) -> i32 {
    eprintln!("error: {message}\nrun `herdmux park help` for usage");
    EXIT_USAGE
}

/// Print `message` and show it on `client`, since a park started from the
/// sidebar runs detached with nowhere else to report.
fn report(client: Option<&str>, message: &str, is_error: bool) {
    if is_error {
        eprintln!("error: {message}");
    } else {
        println!("{message}");
    }
    if let Some(client) = client {
        let text = format!("herdmux: {message}");
        let _ = tmux::run_tmux(&["display-message", "-c", client, "-d", "5000", &text]);
    }
}

// ─── park ───────────────────────────────────────────────────────────────────

fn run_park(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &["client"],
        switches: &["no-handoff"],
    };
    let parsed = match Args::parse(raw, &SPEC) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let [query] = parsed.positionals.as_slice() else {
        return usage_error("park takes exactly one <target>");
    };
    let client = parsed.value("client");
    let Some(dir) = park::parked_dir() else {
        report(client, "no state directory (HOME is not set)", true);
        return EXIT_ERROR;
    };
    let loc = match target::lookup(query) {
        Ok(loc) => loc,
        Err(e) => {
            report(client, &e, true);
            return EXIT_ERROR;
        }
    };
    let tmux = RealTmux;
    let state = tmux.pane_state(&loc.pane_id);
    let known = agent_known(&tmux, state.as_ref());
    let Some(state) = state else {
        report(client, &format!("{} is gone", loc.pane_id), true);
        return EXIT_ERROR;
    };
    if let Err(e) = check_parkable(&state, known, &self_pane()) {
        report(client, &e, true);
        return EXIT_REFUSED;
    }
    let mut parked = match snapshot(&loc) {
        Ok(p) => p,
        Err(e) => {
            report(
                client,
                &format!("{} can't be resumed later: {e}", loc.pane_id),
                true,
            );
            return EXIT_REFUSED;
        }
    };

    let last_reply = || current_reply(&loc.pane_id, &loc.session_id);
    if parsed.has("no-handoff") {
        parked.handoff = last_reply().unwrap_or_default();
    } else {
        match ask_handoff(&tmux, &state, &|| {
            crate::activity::read_response(&loc.pane_id)
        }) {
            Ok(text) => {
                parked.handoff = text;
                parked.handoff_from_agent = true;
            }
            Err(e) => {
                eprintln!("note: no handoff ({e}); keeping the last reply instead");
                parked.handoff = last_reply().unwrap_or_default();
            }
        }
    }

    if let Err(e) = park::save(&dir, &parked) {
        report(
            client,
            &format!("could not save the parked session: {e}"),
            true,
        );
        return EXIT_ERROR;
    }
    let exited = exit_agent(&tmux, &loc.pane_id, &loc.agent);
    close_pane(&loc.pane_id, exited);
    report(client, &format!("parked {}", parked.name()), false);
    EXIT_OK
}

/// Why a pane can't be parked. On top of the prompt rules (not self, not
/// the sidebar, an agent that is not busy or blocked), a live background
/// shell would die with the pane.
pub(super) fn check_parkable(
    state: &PaneState,
    agent_known: bool,
    self_pane: &str,
) -> Result<(), String> {
    check_promptable(state, agent_known, self_pane, false)?;
    if state.status == "background" {
        return Err(format!(
            "{} has a background shell running; parking would stop it",
            state.pane_id
        ));
    }
    if state.session_id.is_empty() {
        return Err(format!(
            "{} has not reported a session id yet, so it could not be resumed",
            state.pane_id
        ));
    }
    Ok(())
}

/// Everything the record needs except the handoff. Fails when the session
/// could not be resumed, before anything is sent to the agent.
fn snapshot(loc: &PaneLocation) -> Result<Parked, String> {
    let processes = crate::process::ProcessSnapshot::scan().ok_or("could not read processes")?;
    let pid = processes
        .interactive_agent_pid(loc.pane_pid, &loc.agent)
        .ok_or("the agent is not the pane's program")?;
    let invocation = crate::process::read_invocation(pid).ok_or("could not read its command")?;
    let cwd = crate::process::process_cwd(pid).unwrap_or_else(|| loc.effective_cwd().into());
    let mut parked = Parked {
        session_id: loc.session_id.clone(),
        agent: loc.agent.clone(),
        argv: crate::process::normalize_agent_argv(&invocation.argv, &loc.agent),
        argv_exact: invocation.argv_exact,
        env: invocation.env,
        parked_at: crate::time::now_epoch_secs(),
        tmux_session: loc.session.clone(),
        window_name: tmux::display_message(&loc.pane_id, "#{window_name}"),
        desc: loc.desc.clone(),
        pinned: loc.pinned,
        last_prompt: if loc.prompt_source == "user" {
            loc.prompt.clone()
        } else {
            String::new()
        },
        repo: crate::git::repo_root(&cwd)
            .as_deref()
            .and_then(|root| Path::new(root).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        branch: if loc.worktree_branch.is_empty() {
            crate::git::run_git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default()
        } else {
            loc.worktree_branch.clone()
        },
        cwd,
        ..Default::default()
    };
    if let ResumeCommand::Skip(reason) = crate::resume::build_command(&parked.resume_entry()) {
        return Err(reason);
    }
    if parked.agent == "claude" {
        add_digest(&mut parked);
    }
    Ok(parked)
}

/// Claude's title and the typed prompts, read before the handoff prompt
/// joins the transcript.
fn add_digest(parked: &mut Parked) {
    let mut dirs = crate::transcript::config_dirs();
    if let Some((_, dir)) = parked.env.iter().find(|(k, _)| k == "CLAUDE_CONFIG_DIR") {
        dirs.insert(0, dir.into());
    }
    let Some(path) = crate::transcript::find_transcript(&parked.session_id, &parked.cwd, &dirs)
    else {
        return;
    };
    let mut tail = crate::transcript::TranscriptTail::new(path);
    tail.poll();
    let digest = &tail.digest;
    parked.title = digest.title.clone();
    parked.goal = digest.goal().unwrap_or_default().to_string();
    if let Some(last) = digest.last_prompt() {
        parked.last_prompt = last.to_string();
    }
}

/// The pane's last reply, unless it belongs to an earlier session.
fn current_reply(pane: &str, session_id: &str) -> Option<String> {
    super::read::current_reply(crate::activity::read_response(pane), session_id)
        .ok()
        .map(|r| r.message.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Send the handoff prompt and return the agent's answer. A turn that
/// asks for a permission or runs past the timeout is stopped with Escape.
pub(super) fn ask_handoff<T: AgentTmux>(
    tmux: &T,
    state: &PaneState,
    read_reply: &dyn Fn() -> Option<AgentResponse>,
) -> Result<String, String> {
    let pane = &state.pane_id;
    let since = match submit(tmux, state, HANDOFF_PROMPT)? {
        Submit::Confirmed(at) => at,
        Submit::Unconfirmable | Submit::NotSubmitted => {
            return Err("the agent did not take the handoff prompt".into());
        }
    };
    let stop_turn = || {
        let _ = tmux.press_escape(pane);
        let _ = wait_until(
            tmux,
            pane,
            Until::Stop,
            Some(since),
            Some(SETTLE_TIMEOUT_MS),
        );
    };
    match wait_until(
        tmux,
        pane,
        Until::Stop,
        Some(since),
        Some(HANDOFF_TIMEOUT_MS),
    ) {
        Outcome::Done => read_reply()
            .filter(|r| r.prompt_at_ms.is_none_or(|at| at >= since))
            .map(|r| r.message.trim().to_string())
            .filter(|m| !m.is_empty())
            .ok_or_else(|| "the agent ended its turn without a reply".into()),
        Outcome::Blocked(reason) => {
            stop_turn();
            Err(format!("the agent asked for a permission ({reason})"))
        }
        Outcome::TimedOut => {
            stop_turn();
            Err("the agent did not answer within 2 minutes".into())
        }
        Outcome::Error(e) => Err(format!("the agent failed ({e})")),
        Outcome::Gone | Outcome::Pending => Err("the pane closed".into()),
    }
}

/// End the agent with its own exit command, so it closes its transcript
/// cleanly. True once no agent runs in the pane.
pub(super) fn exit_agent<T: AgentTmux>(tmux: &T, pane: &str, agent: &str) -> bool {
    let Some(state) = tmux.pane_state(pane) else {
        return true;
    };
    let command = if agent == "codex" { "/quit" } else { "/exit" };
    let _ = submit(tmux, &state, command);
    let deadline = tmux.now_ms() + EXIT_TIMEOUT_MS;
    loop {
        let state = tmux.pane_state(pane);
        if !agent_known(tmux, state.as_ref()) {
            return true;
        }
        if tmux.now_ms() >= deadline {
            return false;
        }
        tmux.sleep(POLL);
    }
}

/// Close the pane, then its window when only a sidebar is left. A pane
/// that is all its session has stays open as a shell: closing it would end
/// the session and detach its terminals.
fn close_pane(pane: &str, agent_exited: bool) {
    let window = tmux::display_message(pane, "#{window_id}");
    let roles = tmux::run_tmux(&["list-panes", "-t", pane, "-F", "#{pane_id}:#{@pane_role}"]);
    let windows = tmux::display_message(pane, "#{session_windows}")
        .parse()
        .ok();
    if keep_last_pane(roles.as_deref(), windows) {
        if !agent_exited {
            let _ = tmux::send_key(pane, "C-c");
        }
        return;
    }
    let _ = tmux::kill_pane(pane);
    if !window.is_empty() {
        crate::cli::toggle::cmd_auto_close(&[window]);
    }
}

/// True when the pane is the only non-sidebar pane of its session's only
/// window, or tmux could not say. `window_roles` holds `pane_id:role` lines.
fn keep_last_pane(window_roles: Option<&str>, session_windows: Option<u32>) -> bool {
    let Some(roles) = window_roles else {
        return true;
    };
    let others = roles
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_end().ends_with(":sidebar"))
        .count()
        > 1;
    !others && session_windows.is_none_or(|n| n <= 1)
}

// ─── resume / forget ────────────────────────────────────────────────────────

fn run_resume(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &["client"],
        switches: &[],
    };
    let parsed = match Args::parse(raw, &SPEC) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let [session_id] = parsed.positionals.as_slice() else {
        return usage_error("park resume takes exactly one <session_id>");
    };
    let client = parsed.value("client");
    let Some(dir) = park::parked_dir() else {
        report(client, "no state directory (HOME is not set)", true);
        return EXIT_ERROR;
    };
    match resume_parked(&dir, session_id, client) {
        Ok(message) => {
            report(client, &message, false);
            EXIT_OK
        }
        Err(e) => {
            report(client, &e, true);
            EXIT_ERROR
        }
    }
}

fn resume_parked(dir: &Path, session_id: &str, client: Option<&str>) -> Result<String, String> {
    let parked = park::load(dir, session_id).ok_or("no parked session with that id")?;
    if !Path::new(&parked.cwd).is_dir() {
        return Err(format!("{} no longer exists", parked.cwd));
    }
    let argv = match crate::resume::build_command(&parked.resume_entry()) {
        ResumeCommand::Resume(argv) | ResumeCommand::Fresh(argv) => argv,
        ResumeCommand::Skip(reason) => return Err(reason),
    };
    let command = crate::resume::render_command(&parked.env, &argv);
    let session = target_session(&parked.tmux_session, client)?;
    let name = if parked.window_name.is_empty() {
        parked.agent.as_str()
    } else {
        parked.window_name.as_str()
    };
    let (pane, _) = tmux::new_window(&session, &parked.cwd, name, true)?;
    if !parked.desc.is_empty() {
        tmux::set_pane_option(&pane, tmux::PANE_DESC, &parked.desc);
    }
    if parked.pinned {
        tmux::set_pane_option(&pane, tmux::PANE_PINNED, "1");
    }
    tmux::show_pane(&pane, client);
    crate::resume::wait_for_shell(&pane);
    tmux::send_command(&pane, &command)?;
    if registered(&pane) {
        park::remove(dir, session_id).map_err(|e| format!("resumed, but {e}"))?;
        Ok(format!("resumed {}", parked.name()))
    } else {
        Ok(format!(
            "started {} in {pane}; it has not reported in yet, so it stays parked",
            parked.name()
        ))
    }
}

/// The parked session's tmux session when it still exists, else the one
/// `client` shows.
fn target_session(parked_session: &str, client: Option<&str>) -> Result<String, String> {
    let exact = format!("={parked_session}");
    if !parked_session.is_empty() && tmux::run_tmux(&["has-session", "-t", &exact]).is_some() {
        return Ok(parked_session.to_string());
    }
    let mut args = vec!["display-message", "-p"];
    if let Some(client) = client {
        args.extend(["-c", client]);
    }
    args.push("#{session_name}");
    tmux::run_tmux(&args)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no tmux session to open it in".into())
}

/// Wait until the agent in `pane` reports a session id.
fn registered(pane: &str) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < REGISTER_TIMEOUT {
        if !tmux::get_pane_option_value(pane, tmux::PANE_SESSION_ID).is_empty() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

fn run_forget(raw: &[String]) -> i32 {
    let [session_id] = raw else {
        return usage_error("park forget takes exactly one <session_id>");
    };
    let Some(dir) = park::parked_dir() else {
        eprintln!("error: no state directory (HOME is not set)");
        return EXIT_ERROR;
    };
    match park::remove(&dir, session_id) {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::env::fake::FakeTmux;
    use super::*;

    fn idle_state() -> PaneState {
        PaneState {
            pane_id: "%5".into(),
            agent: "claude".into(),
            status: "idle".into(),
            prompt_at: Some(500),
            session_id: "sid-5".into(),
            pane_pid: 42,
            ..Default::default()
        }
    }

    fn reply(prompt_at_ms: u64, message: &str) -> AgentResponse {
        AgentResponse {
            session_id: "sid-5".into(),
            prompt_at_ms: Some(prompt_at_ms),
            message: message.into(),
            ..Default::default()
        }
    }

    #[test]
    fn idle_agents_with_a_session_can_be_parked() {
        assert!(check_parkable(&idle_state(), true, "%1").is_ok());
    }

    #[test]
    fn busy_blocked_background_and_unregistered_agents_are_refused() {
        let with = |f: fn(&mut PaneState)| {
            let mut s = idle_state();
            f(&mut s);
            check_parkable(&s, true, "%1").unwrap_err()
        };
        assert!(with(|s| s.status = "running".into()).contains("busy"));
        assert!(
            with(|s| {
                s.status = "waiting".into();
                s.wait_reason = "permission_prompt".into();
            })
            .contains("permission")
        );
        assert!(with(|s| s.status = "background".into()).contains("background shell"));
        assert!(with(|s| s.session_id.clear()).contains("session id"));
        assert!(check_parkable(&idle_state(), true, "%5").is_err(), "self");
        assert!(
            check_parkable(&idle_state(), false, "%1").is_err(),
            "no agent"
        );
    }

    #[test]
    fn handoff_returns_the_reply_to_the_handoff_prompt() {
        let fake = FakeTmux {
            submit_on_enter: Some(1),
            ..FakeTmux::with_state(idle_state())
        };
        let mut done = idle_state();
        done.prompt_at = Some(1_300);
        fake.at(2_000, Some(done));
        let text = ask_handoff(&fake, &idle_state(), &|| {
            Some(reply(1_300, " Goal: a\nNext: b "))
        })
        .unwrap();
        assert_eq!(text, "Goal: a\nNext: b");
        assert!(fake.calls()[0].starts_with("paste(%5,[herdmux park]"));
    }

    #[test]
    fn handoff_ignores_a_reply_from_an_earlier_turn() {
        let fake = FakeTmux {
            submit_on_enter: Some(1),
            ..FakeTmux::with_state(idle_state())
        };
        let mut done = idle_state();
        done.prompt_at = Some(1_300);
        fake.at(2_000, Some(done));
        let err = ask_handoff(&fake, &idle_state(), &|| Some(reply(400, "old"))).unwrap_err();
        assert!(err.contains("without a reply"));
    }

    #[test]
    fn a_permission_request_during_the_handoff_is_escaped() {
        let fake = FakeTmux {
            submit_on_enter: Some(1),
            ..FakeTmux::with_state(idle_state())
        };
        let mut blocked = idle_state();
        blocked.status = "waiting".into();
        blocked.wait_reason = "permission_prompt".into();
        blocked.prompt_at = Some(1_300);
        fake.at(2_000, Some(blocked));
        let err = ask_handoff(&fake, &idle_state(), &|| None).unwrap_err();
        assert!(err.contains("permission"));
        assert!(fake.calls().contains(&"escape(%5)".to_string()));
    }

    #[test]
    fn a_handoff_that_runs_too_long_is_escaped() {
        let fake = FakeTmux {
            submit_on_enter: Some(1),
            ..FakeTmux::with_state(idle_state())
        };
        let err = ask_handoff(&fake, &idle_state(), &|| None).unwrap_err();
        assert!(err.contains("2 minutes"));
        assert!(fake.calls().contains(&"escape(%5)".to_string()));
    }

    #[test]
    fn exit_waits_until_the_agent_is_gone() {
        let fake = FakeTmux::with_state(idle_state());
        let mut shell = idle_state();
        shell.agent.clear();
        fake.at(1_000, Some(shell));
        assert!(exit_agent(&fake, "%5", "claude"));
        assert!(fake.calls().contains(&"paste(%5,/exit)".to_string()));
    }

    #[test]
    fn codex_exits_with_quit_and_a_stuck_agent_times_out() {
        let fake = FakeTmux::with_state(PaneState {
            agent: "codex".into(),
            ..idle_state()
        });
        assert!(!exit_agent(&fake, "%5", "codex"));
        assert!(fake.calls().contains(&"paste(%5,/quit)".to_string()));
    }

    #[test]
    fn the_last_pane_of_a_session_stays_open() {
        // Alone in the only window: keep it.
        assert!(keep_last_pane(Some("%5:"), Some(1)));
        // Next to a sidebar in the only window: still the last real pane.
        assert!(keep_last_pane(Some("%1:sidebar\n%5:"), Some(1)));
        // Another pane in the window, or another window: close it.
        assert!(!keep_last_pane(Some("%5:\n%6:"), Some(1)));
        assert!(!keep_last_pane(Some("%1:sidebar\n%5:"), Some(3)));
        // tmux did not answer: keep it.
        assert!(keep_last_pane(None, Some(3)));
    }
}
