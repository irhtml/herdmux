//! `agent` subcommand: lets one coding agent list, spawn, prompt, wait on
//! and read the other agents running in tmux panes. State comes from the
//! same `@pane_*` options the hooks maintain for the sidebar, so this is
//! a thin control layer, not a second source of truth.

mod env;
mod list;
mod prompt;
mod read;
mod spawn;
mod target;
mod wait;

const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_BLOCKED: i32 = 3;
const EXIT_AGENT_ERROR: i32 = 4;
const EXIT_GONE: i32 = 5;
const EXIT_REFUSED: i32 = 6;
const EXIT_NOT_SUBMITTED: i32 = 7;
const EXIT_TIMEOUT: i32 = 124;

/// Default `--timeout` in seconds. Stays under the 2 minute limit agent
/// shells put on a single tool call, so a long wait ends with a clean
/// exit 124 instead of being killed mid-poll.
const DEFAULT_TIMEOUT_SECS: u64 = 110;

const USAGE: &str = "\
usage: tmux-agent-sidebar agent <command> [options]

Drive other coding agents running in tmux panes.

commands:
  list [--json] [--all]
      Agent panes with status, tag, spawning pane and cwd. `*` / \"self\": true
      marks the caller.
  spawn [--agent A] [--mode M] [--cwd DIR | --worktree NAME] [--window]
        [--desc TAG] [--prompt TEXT [--wait] [--timeout S]] [-- AGENT_ARGS...]
      Start an agent in a new split (or window) without moving focus and
      print its pane id. With --prompt, waits until it is ready and sends it.
  prompt <target> <text | -> [--wait] [--timeout S] [--force]
      Submit a prompt (`-` reads stdin, or pass the text after `--`).
      Refuses busy or blocked agents unless --force. With --wait, prints
      the reply once the agent stops.
  wait <target> [--until stop|done] [--timeout S] [--since MS]
      Block until the agent stops. `stop` (default) also returns when it is
      blocked on a permission prompt; `done` keeps waiting through it.
  read <target> [--response [--json] | --screen [--lines N]]
      The agent's last full reply (default) or the bottom of its screen.

targets: pane id (%12), session:window.pane, a @pane_desc tag, or a worktree name.
--timeout defaults to 110 seconds; 0 waits forever.

exit codes:
  0 done   1 error   2 usage   3 blocked on permission   4 agent error
  5 agent gone   6 refused (busy, blocked, self)   7 prompt not submitted
  124 timed out (the agent keeps running: wait again, do not re-send)";

pub(crate) fn cmd_agent(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "list" | "ls" => list::run(rest),
        "spawn" => spawn::run(rest),
        "prompt" | "send" => prompt::run(rest),
        "wait" => wait::run(rest),
        "read" => read::run(rest),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            EXIT_OK
        }
        other => {
            eprintln!("unknown agent command `{other}`\n\n{USAGE}");
            EXIT_USAGE
        }
    }
}

/// Parse `--timeout` (seconds, `0` = no limit) into milliseconds.
fn timeout_ms(parsed: &super::args::Args) -> Result<Option<u64>, String> {
    let secs = parsed.number("timeout")?.unwrap_or(DEFAULT_TIMEOUT_SECS);
    Ok((secs > 0).then_some(secs * 1000))
}

fn self_pane() -> String {
    super::tmux_pane()
}

fn usage_error(message: &str) -> i32 {
    eprintln!("error: {message}\nrun `tmux-agent-sidebar agent help` for usage");
    EXIT_USAGE
}
