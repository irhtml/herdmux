//! `agent prompt`: paste a prompt into another agent's input and submit it.

use std::io::Read;
use std::time::Duration;

use super::env::{AgentTmux, PaneState, RealTmux};
use super::wait::{Outcome, Until, agent_known, report, wait_until};
use super::{
    EXIT_ERROR, EXIT_GONE, EXIT_NOT_SUBMITTED, EXIT_OK, EXIT_REFUSED, self_pane, target,
    timeout_ms, usage_error,
};
use crate::cli::args::{Args, Spec};
use crate::cli::hook::is_permission_wait_reason;

/// Pause between the paste and Enter so the agent's input box has taken
/// the whole bracketed paste before the submit key arrives.
const PASTE_SETTLE: Duration = Duration::from_millis(300);
const CONFIRM_POLL: Duration = Duration::from_millis(100);
const CONFIRM_WINDOW_MS: u64 = 3_000;

/// Why a pane must not receive a prompt. Self, sidebar and non-agent
/// panes are refused even with `--force`.
pub(super) fn check_promptable(
    state: &PaneState,
    agent_known: bool,
    self_pane: &str,
    force: bool,
) -> Result<(), String> {
    let pane = &state.pane_id;
    if !self_pane.is_empty() && pane == self_pane {
        return Err(format!(
            "{pane} is your own pane; prompting yourself would deadlock"
        ));
    }
    if state.role == "sidebar" {
        return Err(format!("{pane} is the sidebar, not an agent"));
    }
    if !agent_known {
        return Err(format!("{pane} is not running an agent"));
    }
    if force {
        return Ok(());
    }
    match state.status.as_str() {
        "running" => Err(format!(
            "{pane} is busy; `agent wait {pane}` first, or pass --force to queue the prompt"
        )),
        "waiting" if is_permission_wait_reason(&state.wait_reason) => Err(format!(
            "{pane} is blocked on a permission prompt ({}); a prompt would answer it. \
             Ask the user to decide, or pass --force",
            state.wait_reason
        )),
        "waiting" => Err(format!(
            "{pane} is waiting ({}); pass --force to prompt anyway",
            state.wait_reason
        )),
        _ => Ok(()),
    }
}

/// Slash commands and `!` shell escapes are handled by the agent's UI
/// and never reach the UserPromptSubmit hook, so they can't be confirmed.
fn is_ui_command(text: &str) -> bool {
    text.starts_with('/') || text.starts_with('!')
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Submit {
    /// The hook recorded the prompt at this epoch-ms timestamp.
    Confirmed(u64),
    /// Sent, but of a kind no hook reports.
    Unconfirmable,
    NotSubmitted,
}

pub(super) fn submit<T: AgentTmux>(
    tmux: &T,
    state: &PaneState,
    text: &str,
) -> Result<Submit, String> {
    let pane = &state.pane_id;
    if state.in_mode {
        tmux.cancel_mode(pane)?;
    }
    tmux.paste(pane, text)?;
    tmux.sleep(PASTE_SETTLE);
    if is_ui_command(text) {
        tmux.press_enter(pane)?;
        return Ok(Submit::Unconfirmable);
    }
    // A slow hook can miss the first window; one more Enter on an
    // already-submitted prompt only sends an empty line, which agents
    // ignore.
    for _ in 0..2 {
        tmux.press_enter(pane)?;
        if let Some(at) = await_prompt_at(tmux, pane, state.prompt_at) {
            return Ok(Submit::Confirmed(at));
        }
    }
    Ok(Submit::NotSubmitted)
}

fn await_prompt_at<T: AgentTmux>(tmux: &T, pane: &str, before: Option<u64>) -> Option<u64> {
    let deadline = tmux.now_ms() + CONFIRM_WINDOW_MS;
    loop {
        let at = tmux.pane_state(pane)?.prompt_at;
        if at.is_some() && at != before {
            return at;
        }
        if tmux.now_ms() >= deadline {
            return None;
        }
        tmux.sleep(CONFIRM_POLL);
    }
}

/// Submit `text` and optionally wait for the reply. Shared by `prompt`
/// and `spawn --prompt`. Prints the reply (with `wait`) or how to wait
/// for it later.
pub(super) fn deliver<T: AgentTmux>(
    tmux: &T,
    state: &PaneState,
    text: &str,
    wait: bool,
    timeout: Option<u64>,
) -> i32 {
    let pane = &state.pane_id;
    let since = match submit(tmux, state, text) {
        Ok(Submit::Confirmed(at)) => at,
        Ok(Submit::Unconfirmable) => {
            if wait {
                eprintln!("note: `/` and `!` commands don't report back; not waiting");
            }
            eprintln!("sent to {pane}");
            return EXIT_OK;
        }
        Ok(Submit::NotSubmitted) => {
            eprintln!(
                "error: {pane} did not register the prompt. It may be showing a dialog \
                 (trust, login, permissions) or its hooks are not installed."
            );
            if let Ok(screen) = tmux.capture(pane, 15) {
                eprintln!("--- {pane} screen ---\n{screen}");
            }
            return EXIT_NOT_SUBMITTED;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return if tmux.pane_state(pane).is_none() {
                EXIT_GONE
            } else {
                EXIT_ERROR
            };
        }
    };
    // Status chatter goes to stderr: stdout carries only data (the
    // reply, or `spawn`'s pane id) so callers can capture it as is.
    if !wait {
        eprintln!("submitted to {pane}; wait with: agent wait {pane} --since {since}");
        return EXIT_OK;
    }
    let outcome = wait_until(tmux, pane, Until::Stop, Some(since), timeout);
    if outcome == Outcome::Done {
        print_reply(pane, since);
        return EXIT_OK;
    }
    report(tmux, pane, &outcome);
    outcome.exit_code()
}

fn print_reply(pane: &str, since: u64) {
    match crate::activity::read_response(pane) {
        Some(resp)
            if resp.prompt_at_ms.is_none_or(|at| at >= since) && !resp.message.is_empty() =>
        {
            println!("{}", resp.message)
        }
        _ => eprintln!(
            "note: {pane} ended its turn without reply text (it may have been interrupted)"
        ),
    }
}

fn read_text(parsed: &Args) -> Result<String, String> {
    let text = if !parsed.trailing.is_empty() {
        parsed.trailing.join(" ")
    } else {
        match parsed.positionals.get(1).map(String::as_str) {
            Some("-") => {
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| format!("reading stdin: {e}"))?;
                buf
            }
            Some(text) if parsed.positionals.len() == 2 => text.to_string(),
            Some(_) => return Err("quote the prompt text as a single argument".into()),
            None => return Err("prompt needs <target> and <text>".into()),
        }
    };
    let text = text.trim_end_matches(['\n', '\r']).to_string();
    if text.trim().is_empty() {
        return Err("prompt text is empty".into());
    }
    Ok(text)
}

pub(super) fn run(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &["timeout"],
        switches: &["wait", "force"],
    };
    let parsed = match Args::parse(raw, &SPEC) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let Some(query) = parsed.positionals.first() else {
        return usage_error("prompt needs <target> and <text>");
    };
    let (text, timeout) = match (read_text(&parsed), timeout_ms(&parsed)) {
        (Ok(text), Ok(timeout)) => (text, timeout),
        (Err(e), _) | (_, Err(e)) => return usage_error(&e),
    };
    let pane = match target::lookup(query) {
        Ok(loc) => loc.pane_id,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_ERROR;
        }
    };
    let tmux = RealTmux;
    let Some(state) = tmux.pane_state(&pane) else {
        eprintln!("error: {pane} is gone");
        return EXIT_GONE;
    };
    let known = agent_known(&tmux, Some(&state));
    if let Err(reason) = check_promptable(&state, known, &self_pane(), parsed.has("force")) {
        eprintln!("refused: {reason}");
        return EXIT_REFUSED;
    }
    deliver(&tmux, &state, &text, parsed.has("wait"), timeout)
}

#[cfg(test)]
mod tests {
    use super::super::env::fake::FakeTmux;
    use super::*;

    fn idle() -> PaneState {
        PaneState {
            pane_id: "%5".into(),
            agent: "claude".into(),
            status: "idle".into(),
            prompt_at: Some(500),
            ..Default::default()
        }
    }

    #[test]
    fn refuses_self_sidebar_and_non_agents_even_with_force() {
        assert!(check_promptable(&idle(), true, "%5", true).is_err());
        let sidebar = PaneState {
            role: "sidebar".into(),
            ..idle()
        };
        assert!(check_promptable(&sidebar, true, "%1", true).is_err());
        assert!(check_promptable(&idle(), false, "%1", true).is_err());
    }

    #[test]
    fn refuses_busy_and_blocked_unless_forced() {
        let running = PaneState {
            status: "running".into(),
            ..idle()
        };
        let blocked = PaneState {
            status: "waiting".into(),
            wait_reason: "permission_prompt".into(),
            ..idle()
        };
        for state in [&running, &blocked] {
            assert!(check_promptable(state, true, "%1", false).is_err());
            assert!(check_promptable(state, true, "%1", true).is_ok());
        }
        let err = check_promptable(&blocked, true, "%1", false).unwrap_err();
        assert!(err.contains("permission"), "{err}");
        assert!(check_promptable(&idle(), true, "", false).is_ok());
    }

    #[test]
    fn submit_confirms_via_prompt_at() {
        let fake = FakeTmux {
            submit_on_enter: Some(1),
            ..FakeTmux::with_state(idle())
        };
        let submitted = submit(&fake, &idle(), "fix it").unwrap();
        assert!(matches!(submitted, Submit::Confirmed(at) if at > 500));
        assert_eq!(fake.calls(), vec!["paste(%5,fix it)", "enter(%5)"]);
    }

    #[test]
    fn submit_retries_enter_once_then_gives_up() {
        let retried = FakeTmux {
            submit_on_enter: Some(2),
            ..FakeTmux::with_state(idle())
        };
        assert!(matches!(
            submit(&retried, &idle(), "x").unwrap(),
            Submit::Confirmed(_)
        ));

        let stuck = FakeTmux::with_state(idle());
        assert_eq!(submit(&stuck, &idle(), "x").unwrap(), Submit::NotSubmitted);
        assert_eq!(stuck.calls(), vec!["paste(%5,x)", "enter(%5)", "enter(%5)"]);
    }

    #[test]
    fn submit_leaves_copy_mode_first_and_skips_confirm_for_slash_commands() {
        let in_mode = PaneState {
            in_mode: true,
            ..idle()
        };
        let fake = FakeTmux::with_state(in_mode.clone());
        assert_eq!(
            submit(&fake, &in_mode, "/compact").unwrap(),
            Submit::Unconfirmable
        );
        assert_eq!(
            fake.calls(),
            vec!["cancel(%5)", "paste(%5,/compact)", "enter(%5)"]
        );
    }

    #[test]
    fn deliver_waits_for_the_turn_it_submitted() {
        let fake = FakeTmux {
            submit_on_enter: Some(1),
            ..FakeTmux::with_state(idle())
        };
        // The turn ends 5s after the prompt lands.
        fake.at(
            5_500,
            Some(PaneState {
                prompt_at: Some(1_300),
                ..idle()
            }),
        );
        let code = deliver(&fake, &idle(), "go", true, Some(60_000));
        assert_eq!(code, EXIT_OK);
    }
}
