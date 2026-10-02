//! `agent spawn`: start an agent in a new pane without moving focus,
//! optionally hand it a first prompt.

use std::io::Read;
use std::time::Duration;

use super::env::{AgentTmux, RealTmux};
use super::{
    EXIT_ERROR, EXIT_GONE, EXIT_NOT_SUBMITTED, EXIT_OK, self_pane, timeout_ms, usage_error,
};
use crate::cli::args::{Args, Spec};
use crate::{git, tmux, worktree};

const READY_LIMIT_MS: u64 = 60_000;
const READY_POLL: Duration = Duration::from_millis(500);
/// Let the input box finish drawing after the agent reports in.
const SETTLE: Duration = Duration::from_millis(1_500);
/// Hookless readiness (Codex before its first turn): the screen has to
/// stop changing for this long.
const STABLE_SCREEN_MS: u64 = 1_500;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    Ready,
    /// A trust/approval dialog is up. Answering it is the user's call.
    Dialog(String),
    TimedOut,
    Gone,
}

fn shows_dialog(screen: &str) -> bool {
    screen.to_ascii_lowercase().contains("trust")
}

/// Wait until `agent` in `pane` can take a prompt. Claude reports in via
/// SessionStart at launch. Codex only fires hooks once a turn starts, so
/// for it (and OpenCode) a live process with a settled screen also counts.
/// Any trust dialog stops the wait: a pasted prompt plus Enter would
/// accept it on the user's behalf.
pub(super) fn await_ready<T: AgentTmux>(
    tmux: &T,
    pane: &str,
    agent: &str,
    limit_ms: u64,
) -> Readiness {
    let deadline = tmux.now_ms() + limit_ms;
    let hook_only = agent == "claude";
    let mut stable: Option<(String, u64)> = None;
    loop {
        let Some(state) = tmux.pane_state(pane) else {
            return Readiness::Gone;
        };
        let screen = tmux.capture(pane, 40).unwrap_or_default();
        if shows_dialog(&screen) {
            return Readiness::Dialog(screen);
        }
        if state.agent == agent && !state.status.is_empty() {
            tmux.sleep(SETTLE);
            return Readiness::Ready;
        }
        if !hook_only && tmux.detect_agent(state.pane_pid).as_deref() == Some(agent) {
            match &stable {
                Some((prev, since)) if *prev == screen && !screen.trim().is_empty() => {
                    if tmux.now_ms().saturating_sub(*since) >= STABLE_SCREEN_MS {
                        return Readiness::Ready;
                    }
                }
                _ => stable = Some((screen, tmux.now_ms())),
            }
        }
        if tmux.now_ms() >= deadline {
            return Readiness::TimedOut;
        }
        tmux.sleep(READY_POLL);
    }
}

struct Plan {
    agent: String,
    mode: String,
    cwd: String,
    worktree: Option<String>,
    window: bool,
    desc: Option<String>,
    prompt: Option<String>,
    wait: bool,
    extra_args: Vec<String>,
}

fn plan(parsed: &Args) -> Result<Plan, String> {
    if !parsed.positionals.is_empty() {
        return Err("spawn takes no positional arguments (agent flags go after `--`)".into());
    }
    let agent = parsed
        .value("agent")
        .unwrap_or(worktree::DEFAULT_AGENT)
        .to_string();
    if !worktree::AGENTS.contains(&agent.as_str()) {
        return Err(format!(
            "unknown agent `{agent}` (one of {})",
            worktree::AGENTS.join(", ")
        ));
    }
    let mode = parsed
        .value("mode")
        .unwrap_or(worktree::DEFAULT_MODE)
        .to_string();
    let modes = worktree::modes_for(&agent);
    if !modes.contains(&mode.as_str()) {
        return Err(format!(
            "{agent} has no mode `{mode}` (one of {})",
            modes.join(", ")
        ));
    }
    let cwd = match parsed.value("cwd") {
        Some(dir) => dir.to_string(),
        None => std::env::current_dir()
            .map_err(|e| format!("current directory: {e}"))?
            .to_string_lossy()
            .into_owned(),
    };
    if !std::path::Path::new(&cwd).is_dir() {
        return Err(format!("{cwd} is not a directory"));
    }
    let worktree = parsed.value("worktree").map(str::to_string);
    if worktree.is_some() && parsed.has("window") {
        return Err("--worktree always splits the current window; drop --window".into());
    }
    let prompt = match parsed.value("prompt") {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("reading stdin: {e}"))?;
            Some(buf.trim_end_matches(['\n', '\r']).to_string())
        }
        Some(text) => Some(text.to_string()),
        None => None,
    };
    if prompt.as_deref().is_some_and(|p| p.trim().is_empty()) {
        return Err("--prompt is empty".into());
    }
    if parsed.has("wait") && prompt.is_none() {
        return Err("--wait needs --prompt".into());
    }
    Ok(Plan {
        agent,
        mode,
        cwd,
        worktree,
        window: parsed.has("window"),
        desc: parsed.value("desc").map(str::to_string),
        prompt,
        wait: parsed.has("wait"),
        extra_args: parsed.trailing.clone(),
    })
}

/// Create the pane and type the launch command. Returns the new pane id.
fn launch(plan: &Plan, origin: &str) -> Result<String, String> {
    if let Some(name) = &plan.worktree {
        let repo_root = git::repo_root(&plan.cwd)
            .ok_or_else(|| format!("{} is not in a git repo", plan.cwd))?;
        let outcome = worktree::spawn_detailed(&worktree::SpawnRequest {
            repo_root: repo_root.into(),
            task_name: name.clone(),
            origin_pane: origin.to_string(),
            agent: plan.agent.clone(),
            mode: plan.mode.clone(),
            detached: true,
            extra_args: plan.extra_args.clone(),
        })?;
        return Ok(outcome.pane_id);
    }
    let pane = if plan.window {
        let session = tmux::pane_session_name(origin)
            .ok_or_else(|| format!("could not resolve the session of {origin}"))?;
        let name = plan.desc.as_deref().unwrap_or(&plan.agent);
        tmux::new_window(&session, &plan.cwd, name, true)?.0
    } else {
        let target = tmux::worktree_split_target(origin)?;
        tmux::split_worktree_pane(&target, &plan.cwd, true)?
    };
    let command = worktree::launch_command(&plan.agent, &plan.mode, &plan.extra_args);
    if let Err(e) = tmux::send_command(&pane, &command) {
        let _ = tmux::kill_pane(&pane);
        return Err(e);
    }
    Ok(pane)
}

pub(super) fn run(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &[
            "agent", "mode", "cwd", "worktree", "desc", "prompt", "timeout",
        ],
        switches: &["window", "wait"],
    };
    let parsed = match Args::parse(raw, &SPEC) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    let (plan, budget) = match (plan(&parsed), timeout_ms(&parsed)) {
        (Ok(plan), Ok(budget)) => (plan, budget),
        (Err(e), _) | (_, Err(e)) => return usage_error(&e),
    };
    let origin = self_pane();
    if origin.is_empty() {
        eprintln!("error: TMUX_PANE is not set; run spawn from inside tmux");
        return EXIT_ERROR;
    }

    let pane = match launch(&plan, &origin) {
        Ok(pane) => pane,
        Err(e) => {
            eprintln!("error: spawn failed: {e}");
            return EXIT_ERROR;
        }
    };
    tmux::set_pane_option(&pane, tmux::PANE_SPAWNED_BY, &origin);
    if let Some(desc) = &plan.desc {
        tmux::set_pane_option(&pane, tmux::PANE_DESC, desc);
    }
    // The pane id first, so the caller has it even if the prompt fails.
    println!("{pane}");
    let Some(prompt) = &plan.prompt else {
        return EXIT_OK;
    };

    let tmux = RealTmux;
    let started = tmux.now_ms();
    let ready_limit = budget.map_or(READY_LIMIT_MS, |b| b.min(READY_LIMIT_MS));
    match await_ready(&tmux, &pane, &plan.agent, ready_limit) {
        Readiness::Ready => {}
        Readiness::Dialog(screen) => {
            eprintln!(
                "error: {pane} is showing a trust/approval dialog. Ask the user to answer it, \
                 then run `agent prompt {pane} ...`.\n--- {pane} screen ---\n{screen}"
            );
            return EXIT_NOT_SUBMITTED;
        }
        Readiness::TimedOut => {
            let hint = if plan.agent == "claude" {
                "Claude never reported in; check that the tmux-agent-sidebar plugin hooks are installed."
            } else {
                "the agent never became ready; check the pane."
            };
            eprintln!(
                "error: {pane} is not ready after {}s: {hint}",
                ready_limit / 1000
            );
            if let Ok(screen) = tmux.capture(&pane, 15) {
                eprintln!("--- {pane} screen ---\n{screen}");
            }
            return EXIT_NOT_SUBMITTED;
        }
        Readiness::Gone => {
            eprintln!("error: {pane} closed before the agent started");
            return EXIT_GONE;
        }
    }
    let Some(state) = tmux.pane_state(&pane) else {
        eprintln!("error: {pane} closed before the prompt was sent");
        return EXIT_GONE;
    };
    let remaining = budget.map(|b| b.saturating_sub(tmux.now_ms() - started).max(1_000));
    super::prompt::deliver(&tmux, &state, prompt, plan.wait, remaining)
}

#[cfg(test)]
mod tests {
    use super::super::env::PaneState;
    use super::super::env::fake::FakeTmux;
    use super::*;

    fn bare_shell() -> PaneState {
        PaneState {
            pane_id: "%9".into(),
            pane_pid: 100,
            ..Default::default()
        }
    }

    #[test]
    fn claude_is_ready_once_its_session_start_hook_fires() {
        let fake = FakeTmux::with_state(bare_shell());
        fake.at(
            3_000,
            Some(PaneState {
                agent: "claude".into(),
                status: "idle".into(),
                ..bare_shell()
            }),
        );
        assert_eq!(await_ready(&fake, "%9", "claude", 60_000), Readiness::Ready);
    }

    #[test]
    fn claude_without_hooks_times_out_even_with_a_quiet_screen() {
        let fake = FakeTmux {
            detected: Some("claude".into()),
            ..FakeTmux::with_state(bare_shell())
        };
        fake.screen.borrow_mut().push((0, "> ".into()));
        assert_eq!(
            await_ready(&fake, "%9", "claude", 5_000),
            Readiness::TimedOut
        );
    }

    #[test]
    fn codex_is_ready_when_its_screen_settles() {
        let fake = FakeTmux {
            detected: Some("codex".into()),
            ..FakeTmux::with_state(bare_shell())
        };
        // The TUI keeps redrawing while it boots, then goes quiet.
        for (at, frame) in [(0, "boot 0"), (1_200, "boot 1"), (2_400, "boot 2")] {
            fake.screen.borrow_mut().push((at, frame.into()));
        }
        fake.screen.borrow_mut().push((3_000, "› Ask Codex".into()));
        assert_eq!(await_ready(&fake, "%9", "codex", 60_000), Readiness::Ready);
        // Screen timestamps are absolute on the fake clock.
        assert!(fake.clock.get() >= 3_000 + STABLE_SCREEN_MS);
    }

    #[test]
    fn trust_dialogs_stop_the_wait() {
        let fake = FakeTmux {
            detected: Some("codex".into()),
            ..FakeTmux::with_state(bare_shell())
        };
        fake.screen
            .borrow_mut()
            .push((0, "Do you trust the contents of this directory?".into()));
        assert!(matches!(
            await_ready(&fake, "%9", "codex", 60_000),
            Readiness::Dialog(_)
        ));
    }

    #[test]
    fn closed_pane_is_gone() {
        let fake = FakeTmux::with_state(bare_shell());
        fake.at(1_000, None);
        assert_eq!(await_ready(&fake, "%9", "claude", 60_000), Readiness::Gone);
    }

    fn parse(raw: &[&str]) -> Result<Plan, String> {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        let spec = Spec {
            values: &[
                "agent", "mode", "cwd", "worktree", "desc", "prompt", "timeout",
            ],
            switches: &["window", "wait"],
        };
        plan(&Args::parse(&raw, &spec)?)
    }

    #[test]
    fn plan_validates_agent_mode_and_combinations() {
        let tmp = std::env::temp_dir();
        let cwd = tmp.to_str().unwrap();
        let ok = parse(&[
            "--agent", "codex", "--mode", "auto", "--cwd", cwd, "--", "-m", "o3",
        ])
        .unwrap();
        assert_eq!(ok.agent, "codex");
        assert_eq!(ok.extra_args, vec!["-m", "o3"]);
        assert!(parse(&["--agent", "gemini", "--cwd", cwd]).is_err());
        assert!(parse(&["--agent", "codex", "--mode", "plan", "--cwd", cwd]).is_err());
        assert!(parse(&["--wait", "--cwd", cwd]).is_err());
        assert!(parse(&["--worktree", "x", "--window", "--cwd", cwd]).is_err());
        assert!(parse(&["--cwd", "/definitely/not/here"]).is_err());
        assert!(parse(&["--prompt", "  ", "--cwd", cwd]).is_err());
    }
}
