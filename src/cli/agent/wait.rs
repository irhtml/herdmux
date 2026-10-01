//! `agent wait`: poll a pane's hook-maintained status until its turn ends.

use std::time::Duration;

use super::env::{AgentTmux, PaneState, RealTmux};
use super::{
    EXIT_AGENT_ERROR, EXIT_BLOCKED, EXIT_ERROR, EXIT_GONE, EXIT_OK, EXIT_TIMEOUT, target,
    timeout_ms, usage_error,
};
use crate::cli::args::{Args, Spec};
use crate::cli::hook::is_permission_wait_reason;

const POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Until {
    /// First point the agent needs someone: finished, failed, or blocked.
    Stop,
    /// Only a finished or failed turn; permission prompts are waited out.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    Pending,
    Done,
    Blocked(String),
    Error(String),
    Gone,
    TimedOut,
}

impl Outcome {
    pub(super) fn exit_code(&self) -> i32 {
        match self {
            Outcome::Done => EXIT_OK,
            Outcome::Blocked(_) => EXIT_BLOCKED,
            Outcome::Error(_) => EXIT_AGENT_ERROR,
            Outcome::Gone => EXIT_GONE,
            Outcome::TimedOut => EXIT_TIMEOUT,
            Outcome::Pending => EXIT_ERROR,
        }
    }

    pub(super) fn describe(&self) -> String {
        match self {
            Outcome::Done => "done".into(),
            Outcome::Blocked(reason) => format!("blocked ({reason})"),
            Outcome::Error(reason) if reason.is_empty() => "error".into(),
            Outcome::Error(reason) => format!("error ({reason})"),
            Outcome::Gone => "gone".into(),
            Outcome::TimedOut => "timeout".into(),
            Outcome::Pending => "pending".into(),
        }
    }
}

/// Map one status snapshot to an outcome. `since` (epoch ms) ignores a
/// finished state left over from a turn that started before it, which
/// closes the race between submitting a prompt and the hook flipping the
/// pane to `running`.
pub(super) fn classify(
    state: Option<&PaneState>,
    agent_known: bool,
    until: Until,
    since: Option<u64>,
) -> Outcome {
    let Some(state) = state else {
        return Outcome::Gone;
    };
    if !agent_known {
        return Outcome::Gone;
    }
    let fresh = since.is_none_or(|s| state.prompt_at.is_some_and(|at| at >= s));
    match state.status.as_str() {
        "waiting" if is_permission_wait_reason(&state.wait_reason) => match until {
            Until::Stop => Outcome::Blocked(state.wait_reason.clone()),
            Until::Done => Outcome::Pending,
        },
        // "" = agent process up but no hook has fired yet (Codex before its
        // first turn): not working on anything.
        "idle" | "background" | "" if fresh => Outcome::Done,
        "error" if fresh => Outcome::Error(state.wait_reason.clone()),
        _ => Outcome::Pending,
    }
}

pub(super) fn agent_known<T: AgentTmux>(tmux: &T, state: Option<&PaneState>) -> bool {
    state.is_some_and(|s| !s.agent.is_empty() || tmux.detect_agent(s.pane_pid).is_some())
}

pub(super) fn wait_until<T: AgentTmux>(
    tmux: &T,
    pane: &str,
    until: Until,
    since: Option<u64>,
    timeout_ms: Option<u64>,
) -> Outcome {
    let deadline = timeout_ms.map(|t| tmux.now_ms() + t);
    loop {
        let state = tmux.pane_state(pane);
        let known = agent_known(tmux, state.as_ref());
        let outcome = classify(state.as_ref(), known, until, since);
        if outcome != Outcome::Pending {
            return outcome;
        }
        if deadline.is_some_and(|d| tmux.now_ms() >= d) {
            return Outcome::TimedOut;
        }
        tmux.sleep(POLL);
    }
}

/// Print the outcome on stdout and, when the caller has to act, context
/// on stderr: the permission prompt on screen, or how to keep waiting.
pub(super) fn report<T: AgentTmux>(tmux: &T, pane: &str, outcome: &Outcome) {
    println!("{}", outcome.describe());
    match outcome {
        Outcome::Blocked(_) => {
            eprintln!(
                "{pane} is waiting for a permission decision. Ask the user to answer it in \
                 that pane; do not send a prompt (it would answer the dialog)."
            );
            if let Ok(screen) = tmux.capture(pane, 15) {
                eprintln!("--- {pane} screen ---\n{screen}");
            }
        }
        Outcome::TimedOut => eprintln!(
            "{pane} is still working. Run `agent wait {pane}` again; do not re-send the prompt."
        ),
        _ => {}
    }
}

pub(super) fn run(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &["until", "timeout", "since"],
        switches: &[],
    };
    let parsed = match Args::parse(raw, &SPEC) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let [query] = parsed.positionals.as_slice() else {
        return usage_error("wait takes exactly one <target>");
    };
    let until = match parsed.value("until").unwrap_or("stop") {
        "stop" => Until::Stop,
        "done" => Until::Done,
        other => return usage_error(&format!("--until must be stop or done, got `{other}`")),
    };
    let (timeout, since) = match (timeout_ms(&parsed), parsed.number("since")) {
        (Ok(t), Ok(s)) => (t, s),
        (Err(e), _) | (_, Err(e)) => return usage_error(&e),
    };
    let pane = match target::lookup(query) {
        Ok(loc) => loc.pane_id,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_ERROR;
        }
    };
    let outcome = wait_until(&RealTmux, &pane, until, since, timeout);
    report(&RealTmux, &pane, &outcome);
    outcome.exit_code()
}

#[cfg(test)]
mod tests {
    use super::super::env::fake::FakeTmux;
    use super::*;

    fn state(status: &str, reason: &str, prompt_at: Option<u64>) -> PaneState {
        PaneState {
            pane_id: "%5".into(),
            agent: "claude".into(),
            status: status.into(),
            wait_reason: reason.into(),
            prompt_at,
            ..Default::default()
        }
    }

    #[test]
    fn classify_maps_statuses() {
        let c = |s: &PaneState, until| classify(Some(s), true, until, None);
        assert_eq!(
            c(&state("running", "", None), Until::Stop),
            Outcome::Pending
        );
        assert_eq!(c(&state("idle", "", None), Until::Stop), Outcome::Done);
        assert_eq!(
            c(&state("background", "", None), Until::Stop),
            Outcome::Done
        );
        assert_eq!(
            c(&state("error", "rate_limit", None), Until::Stop),
            Outcome::Error("rate_limit".into())
        );
        assert_eq!(
            c(&state("waiting", "permission_prompt", None), Until::Stop),
            Outcome::Blocked("permission_prompt".into())
        );
        assert_eq!(
            c(&state("waiting", "permission_prompt", None), Until::Done),
            Outcome::Pending
        );
        assert_eq!(
            c(&state("waiting", "auth_success", None), Until::Stop),
            Outcome::Pending
        );
    }

    #[test]
    fn classify_reports_gone_panes_and_agents() {
        assert_eq!(classify(None, false, Until::Stop, None), Outcome::Gone);
        assert_eq!(
            classify(Some(&state("idle", "", None)), false, Until::Stop, None),
            Outcome::Gone
        );
    }

    #[test]
    fn classify_ignores_finished_turns_older_than_since() {
        let idle_old = state("idle", "", Some(100));
        assert_eq!(
            classify(Some(&idle_old), true, Until::Stop, Some(200)),
            Outcome::Pending
        );
        let idle_new = state("idle", "", Some(200));
        assert_eq!(
            classify(Some(&idle_new), true, Until::Stop, Some(200)),
            Outcome::Done
        );
        let never = state("idle", "", None);
        assert_eq!(
            classify(Some(&never), true, Until::Stop, Some(200)),
            Outcome::Pending
        );
    }

    #[test]
    fn wait_returns_once_the_turn_finishes() {
        let fake = FakeTmux::with_state(state("running", "", Some(1_000)));
        fake.at(2_000, Some(state("idle", "", Some(1_000))));
        assert_eq!(
            wait_until(&fake, "%5", Until::Stop, Some(1_000), Some(10_000)),
            Outcome::Done
        );
        assert!(fake.clock.get() >= 3_000);
    }

    #[test]
    fn wait_until_done_rides_out_a_permission_prompt() {
        let fake = FakeTmux::with_state(state("waiting", "permission_prompt", Some(1_000)));
        fake.at(1_000, Some(state("running", "", Some(1_000))));
        fake.at(2_000, Some(state("idle", "", Some(1_000))));
        assert_eq!(
            wait_until(&fake, "%5", Until::Done, None, Some(10_000)),
            Outcome::Done
        );
    }

    #[test]
    fn wait_times_out_and_detects_closed_panes() {
        let fake = FakeTmux::with_state(state("running", "", None));
        assert_eq!(
            wait_until(&fake, "%5", Until::Stop, None, Some(1_500)),
            Outcome::TimedOut
        );
        fake.at(0, None);
        assert_eq!(
            wait_until(&fake, "%5", Until::Stop, None, None),
            Outcome::Gone
        );
    }

    #[test]
    fn hookless_agent_process_counts_as_present() {
        let mut fresh_codex = state("", "", None);
        fresh_codex.agent.clear();
        let fake = FakeTmux {
            detected: Some("codex".into()),
            ..FakeTmux::with_state(fresh_codex)
        };
        assert_eq!(
            wait_until(&fake, "%5", Until::Stop, None, Some(1_000)),
            Outcome::Done
        );
    }
}
