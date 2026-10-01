//! The tmux side effects `agent prompt` / `wait` / `spawn` depend on,
//! behind a trait so the polling state machines can be tested against a
//! scripted fake with a virtual clock.

use std::time::Duration;

use crate::process::ProcessSnapshot;
use crate::tmux::{
    self, PANE_AGENT, PANE_PROMPT_AT, PANE_ROLE, PANE_SESSION_ID, PANE_STATUS, PANE_WAIT_REASON,
};

/// Live view of one pane, read with a single `display-message`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PaneState {
    pub(super) pane_id: String,
    pub(super) agent: String,
    pub(super) status: String,
    pub(super) wait_reason: String,
    pub(super) prompt_at: Option<u64>,
    /// Pane is in copy mode (or another tmux mode) and would swallow keys.
    pub(super) in_mode: bool,
    pub(super) session_id: String,
    pub(super) role: String,
    pub(super) pane_pid: u32,
}

const STATE_FIELDS: &[&str] = &[
    "pane_id",
    PANE_AGENT,
    PANE_STATUS,
    PANE_WAIT_REASON,
    PANE_PROMPT_AT,
    "pane_in_mode",
    PANE_SESSION_ID,
    PANE_ROLE,
    "pane_pid",
];

fn state_format() -> String {
    STATE_FIELDS
        .iter()
        .map(|f| format!("#{{q:{f}}}"))
        .collect::<Vec<_>>()
        .join("|")
}

pub(super) fn parse_state(line: &str) -> Option<PaneState> {
    let f = tmux::split_tmux_fields(line.trim_end_matches('\n'), '|');
    if f.len() != STATE_FIELDS.len() || !f[0].starts_with('%') {
        return None;
    }
    Some(PaneState {
        pane_id: f[0].clone(),
        agent: f[1].clone(),
        status: f[2].clone(),
        wait_reason: f[3].clone(),
        prompt_at: f[4].parse().ok(),
        in_mode: f[5] == "1",
        session_id: f[6].clone(),
        role: f[7].clone(),
        pane_pid: f[8].parse().unwrap_or(0),
    })
}

pub(super) trait AgentTmux {
    /// `None` once the pane no longer exists.
    fn pane_state(&self, pane: &str) -> Option<PaneState>;
    /// Agent running interactively in the pane according to the process
    /// tree. Covers the window before the first hook fires (Codex only
    /// fires SessionStart with its first turn).
    fn detect_agent(&self, pane_pid: u32) -> Option<String>;
    fn paste(&self, pane: &str, text: &str) -> Result<(), String>;
    fn press_enter(&self, pane: &str) -> Result<(), String>;
    fn cancel_mode(&self, pane: &str) -> Result<(), String>;
    fn capture(&self, pane: &str, lines: usize) -> Result<String, String>;
    fn sleep(&self, duration: Duration);
    fn now_ms(&self) -> u64;
}

pub(super) struct RealTmux;

impl AgentTmux for RealTmux {
    fn pane_state(&self, pane: &str) -> Option<PaneState> {
        let out =
            tmux::run_tmux_capture(&["display-message", "-t", pane, "-p", &state_format()]).ok()?;
        parse_state(&out)
    }
    fn detect_agent(&self, pane_pid: u32) -> Option<String> {
        let snapshot = ProcessSnapshot::scan()?;
        crate::worktree::AGENTS
            .iter()
            .find(|agent| snapshot.interactive_agent_pid(pane_pid, agent).is_some())
            .map(|agent| agent.to_string())
    }
    fn paste(&self, pane: &str, text: &str) -> Result<(), String> {
        tmux::paste_text(pane, text)
    }
    fn press_enter(&self, pane: &str) -> Result<(), String> {
        tmux::send_key(pane, "Enter")
    }
    fn cancel_mode(&self, pane: &str) -> Result<(), String> {
        tmux::cancel_pane_mode(pane)
    }
    fn capture(&self, pane: &str, lines: usize) -> Result<String, String> {
        tmux::capture_pane(pane, lines)
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn now_ms(&self) -> u64 {
        crate::time::now_epoch_millis()
    }
}

#[cfg(test)]
pub(super) mod fake {
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    use super::{AgentTmux, PaneState};

    /// Scripted pane with a virtual clock. `schedule` swaps in a new state
    /// once the clock reaches its timestamp; `submit_on_enter` makes the
    /// n-th Enter register a prompt the way the UserPromptSubmit hook would.
    #[derive(Default)]
    pub(in crate::cli::agent) struct FakeTmux {
        pub(in crate::cli::agent) state: RefCell<Option<PaneState>>,
        pub(in crate::cli::agent) schedule: RefCell<Vec<(u64, Option<PaneState>)>>,
        pub(in crate::cli::agent) submit_on_enter: Option<usize>,
        pub(in crate::cli::agent) detected: Option<String>,
        pub(in crate::cli::agent) screen: RefCell<Vec<(u64, String)>>,
        pub(in crate::cli::agent) clock: Cell<u64>,
        pub(in crate::cli::agent) enters: Cell<usize>,
        pub(in crate::cli::agent) calls: RefCell<Vec<String>>,
    }

    impl FakeTmux {
        pub(in crate::cli::agent) fn with_state(state: PaneState) -> Self {
            Self {
                state: RefCell::new(Some(state)),
                clock: Cell::new(1_000),
                ..Default::default()
            }
        }

        pub(in crate::cli::agent) fn at(&self, offset_ms: u64, state: Option<PaneState>) {
            let at = self.clock.get() + offset_ms;
            self.schedule.borrow_mut().push((at, state));
        }

        pub(in crate::cli::agent) fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn apply_schedule(&self) {
            let now = self.clock.get();
            let mut schedule = self.schedule.borrow_mut();
            schedule.sort_by_key(|(at, _)| *at);
            while schedule.first().is_some_and(|(at, _)| *at <= now) {
                let (_, state) = schedule.remove(0);
                *self.state.borrow_mut() = state;
            }
        }
    }

    impl AgentTmux for FakeTmux {
        fn pane_state(&self, _pane: &str) -> Option<PaneState> {
            self.apply_schedule();
            self.state.borrow().clone()
        }
        fn detect_agent(&self, _pane_pid: u32) -> Option<String> {
            self.detected.clone()
        }
        fn paste(&self, pane: &str, text: &str) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("paste({pane},{text})"));
            Ok(())
        }
        fn press_enter(&self, pane: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("enter({pane})"));
            let n = self.enters.get() + 1;
            self.enters.set(n);
            if self.submit_on_enter == Some(n)
                && let Some(state) = self.state.borrow_mut().as_mut()
            {
                state.prompt_at = Some(self.clock.get());
                state.status = "running".into();
            }
            Ok(())
        }
        fn cancel_mode(&self, pane: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("cancel({pane})"));
            Ok(())
        }
        fn capture(&self, _pane: &str, _lines: usize) -> Result<String, String> {
            let now = self.clock.get();
            Ok(self
                .screen
                .borrow()
                .iter()
                .rev()
                .find(|(at, _)| *at <= now)
                .map(|(_, s)| s.clone())
                .unwrap_or_default())
        }
        fn sleep(&self, duration: Duration) {
            self.clock
                .set(self.clock.get() + duration.as_millis() as u64);
        }
        fn now_ms(&self) -> u64 {
            self.clock.get()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_state_reads_quoted_fields() {
        let line = "%7|claude|waiting|permission_prompt|1790000000123|1|sid|  |4242\n";
        let state = parse_state(line).unwrap();
        assert_eq!(state.pane_id, "%7");
        assert_eq!(state.agent, "claude");
        assert_eq!(state.status, "waiting");
        assert_eq!(state.wait_reason, "permission_prompt");
        assert_eq!(state.prompt_at, Some(1790000000123));
        assert!(state.in_mode);
        assert_eq!(state.pane_pid, 4242);
    }

    #[test]
    fn parse_state_rejects_error_output() {
        assert_eq!(parse_state("can't find pane: %99"), None);
        assert_eq!(parse_state(""), None);
    }
}
