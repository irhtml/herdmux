//! `resume` subcommand: save agent sessions next to tmux-resurrect's
//! snapshots and relaunch them after a restore. Opt-in with
//! `set -g @sidebar_resume on`; the plugin then wires both commands into
//! resurrect's hooks.

use std::io::Write;

use super::args::{Args, Spec};
use crate::process::ProcessSnapshot;
use crate::resume::{self, Action, ResumeState, ServerInfo};
use crate::time::now_epoch_secs;
use crate::tmux;

const OPTION: &str = "@sidebar_resume";
/// Pause before a detached restore starts typing, so resurrect has
/// finished its own pane work and spinner.
const DETACHED_DELAY_MS: u64 = 500;
const LOG_LIMIT_BYTES: u64 = 256 * 1024;

const USAGE: &str = "\
usage: tmux-agent-sidebar resume <save|restore> [options]

  save [--resurrect-file FILE] [--dry-run] [--quiet]
      Record every agent pane's command line and session id. FILE is the
      tmux-resurrect snapshot to cross-check pane positions against.
      --quiet prints nothing and logs failures (for the resurrect hook).
  restore [--dry-run] [--force] [--detach]
      Relaunch the saved agents with their sessions resumed in the panes
      tmux-resurrect recreated. Runs only within 10 minutes of a tmux
      server start unless --force.

Both do nothing unless `@sidebar_resume` is `on` (--dry-run always works).";

pub(crate) fn cmd_resume(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("save") => save(&args[1..]),
        Some("restore") => restore(&args[1..]),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn enabled() -> bool {
    tmux::get_option(OPTION).as_deref() == Some("on")
}

fn parse(raw: &[String], spec: &Spec) -> Result<Args, i32> {
    match Args::parse(raw, spec) {
        Ok(p) if p.positionals.is_empty() => Ok(p),
        Ok(_) => {
            eprintln!("error: unexpected argument\n\n{USAGE}");
            Err(2)
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            Err(2)
        }
    }
}

/// A snapshot written before this server started must survive until the
/// restore has had its chance: a save in that window (continuum firing
/// early, a manual save) would otherwise replace it with whatever bare
/// shells exist right now.
fn protects_pre_restart_snapshot(
    previous: Option<&ResumeState>,
    server: &ServerInfo,
    now: u64,
) -> bool {
    previous.is_some_and(|p| {
        !p.entries.is_empty()
            && p.saved_at < server.start_time
            && p.restored_at.is_none_or(|at| at < server.start_time)
            && now.saturating_sub(server.start_time) <= resume::RESTORE_WINDOW_SECS
    })
}

fn save(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &["resurrect-file"],
        switches: &["dry-run", "force", "quiet"],
    };
    let parsed = match parse(raw, &SPEC) {
        Ok(p) => p,
        Err(code) => return code,
    };
    // Resurrect runs the hook inside `run-shell`, where any output pops
    // up over the user's pane: hook mode logs errors and says nothing.
    let quiet = parsed.has("quiet");
    match run_save(&parsed) {
        Ok(Some(message)) if !quiet => println!("{message}"),
        Ok(_) => {}
        Err(message) if quiet => append_log(&[format!("save failed: {message}")]),
        Err(message) => {
            eprintln!("error: {message}");
            return 1;
        }
    }
    0
}

/// `Ok(Some(summary))` after a write or a deliberate no-op, `Ok(None)`
/// after a dry run printed its report.
fn run_save(parsed: &Args) -> Result<Option<String>, String> {
    let dry_run = parsed.has("dry-run");
    let force = parsed.has("force");
    if !dry_run && !force && !enabled() {
        return Ok(Some(format!(
            "resume is off; enable it with `set -g {OPTION} on`"
        )));
    }
    let server = ServerInfo::query().ok_or("cannot reach the tmux server")?;
    let path = resume::state_path(&server.socket_path)
        .ok_or("cannot resolve a state directory (HOME unset?)")?;
    let snapshot = match parsed.value("resurrect-file") {
        Some(file) => Some(resume::Snapshot::parse(
            &std::fs::read_to_string(file)
                .map_err(|e| format!("reading {file}: {e}; kept the previous save"))?,
        )),
        None => None,
    };
    let panes = tmux::query_pane_locations();
    if panes.is_empty() {
        return Err("no panes found; kept the previous save".into());
    }
    let now = now_epoch_secs();
    let previous = ResumeState::load(&path);
    if !dry_run && !force && protects_pre_restart_snapshot(previous.as_ref(), &server, now) {
        return Ok(Some(
            "kept the snapshot from before this server started until restore has run".into(),
        ));
    }
    let env = resume::RealSaveEnv {
        processes: ProcessSnapshot::scan(),
    };
    let report = resume::collect(
        &env,
        &panes,
        snapshot.as_ref(),
        previous.as_ref(),
        &server.socket_path,
        now,
    );
    if dry_run {
        print_save_report(&report);
        return Ok(None);
    }
    report
        .state
        .write(&path)
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(Some(format!(
        "saved {} agent session(s), {} skipped, {} tag(s) to {}",
        report.state.entries.len(),
        report.skipped.len(),
        report.state.tags.len(),
        path.display()
    )))
}

fn print_save_report(report: &resume::SaveReport) {
    for entry in &report.state.entries {
        let command = match resume::build_command(entry) {
            resume::ResumeCommand::Resume(argv) => resume::render_command(&entry.env, &argv),
            resume::ResumeCommand::Fresh(argv) => {
                format!("{} (fresh)", resume::render_command(&entry.env, &argv))
            }
            resume::ResumeCommand::Skip(reason) => format!("(skip: {reason})"),
        };
        println!(
            "save {}:{}.{} [{}] {command}",
            entry.session, entry.window_index, entry.pane_index, entry.cwd
        );
    }
    for (address, reason) in &report.skipped {
        println!("skip {address}: {reason}");
    }
    for tag in &report.state.tags {
        println!(
            "tag  {}:{}.{} {}",
            tag.session, tag.window_index, tag.pane_index, tag.desc
        );
    }
    if report.carried > 0 {
        println!("{} pending relaunch(es) carried over", report.carried);
    }
}

fn restore(raw: &[String]) -> i32 {
    const SPEC: Spec = Spec {
        values: &["delay-ms"],
        switches: &["dry-run", "force", "detach"],
    };
    let parsed = match parse(raw, &SPEC) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let dry_run = parsed.has("dry-run");
    let force = parsed.has("force");
    if !dry_run && !force && !enabled() {
        if !parsed.has("detach") {
            eprintln!("resume is off; enable it with `set -g {OPTION} on`");
        }
        return 0;
    }
    if parsed.has("detach") && !dry_run {
        let mut args = vec![
            "resume".to_string(),
            "restore".to_string(),
            "--delay-ms".to_string(),
            DETACHED_DELAY_MS.to_string(),
        ];
        if force {
            args.push("--force".into());
        }
        return match spawn_detached(&args) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: could not start a background restore: {e}");
                1
            }
        };
    }
    match parsed.number("delay-ms") {
        Ok(Some(ms)) => std::thread::sleep(std::time::Duration::from_millis(ms)),
        Ok(None) => {}
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    }

    let Some(server) = ServerInfo::query() else {
        eprintln!("error: cannot reach the tmux server");
        return 1;
    };
    let Some(path) = resume::state_path(&server.socket_path) else {
        eprintln!("error: cannot resolve a state directory (HOME unset?)");
        return 1;
    };
    let Some(state) = ResumeState::load(&path) else {
        println!("nothing to restore ({} not found)", path.display());
        return 0;
    };
    let now = now_epoch_secs();
    let live = tmux::query_pane_locations();
    let plan = match resume::plan(&state, &live, &server, now, force) {
        Ok(plan) => plan,
        Err(e) => {
            append_log(&[format!("not restoring: {e}")]);
            eprintln!("not restoring: {e}");
            return 1;
        }
    };

    if dry_run {
        for action in &plan.actions {
            match action {
                Action::Launch {
                    pane_id,
                    address,
                    command,
                    resumed,
                } => {
                    let verb = if *resumed { "resume" } else { "fresh " };
                    println!("{verb} {address} {pane_id}: {command}");
                }
                Action::Skip { address, reason } => println!("skip   {address}: {reason}"),
            }
        }
        for (pane, desc) in &plan.tags {
            println!("tag    {pane}: {desc}");
        }
        return 0;
    }

    let log = resume::execute(&plan, now);
    append_log(&log);
    // Lets the next save replace the pre-restart snapshot right away.
    let restored = ResumeState {
        restored_at: Some(now),
        ..state
    };
    if let Err(e) = restored.write(&path) {
        append_log(&[format!("could not mark the snapshot restored: {e}")]);
    }
    let skipped = plan.actions.len() - plan.launches();
    let summary = format!(
        "tmux-agent-sidebar: relaunched {} agent(s), {} skipped",
        plan.launches(),
        skipped
    );
    let _ = tmux::run_tmux(&["display-message", &summary]);
    println!("{summary}");
    0
}

fn append_log(lines: &[String]) {
    let Some(dir) = resume::state_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("restore.log");
    let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT_BYTES);
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true);
    if too_big {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut file) = opts.open(&path) {
        let _ = writeln!(file, "--- {}", now_epoch_secs());
        for line in lines {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// Re-run this binary in its own session so the restore outlives the
/// tmux-resurrect script (and its spinner) that invoked it.
fn spawn_detached(args: &[String]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(start: u64) -> ServerInfo {
        ServerInfo {
            socket_path: "/s".into(),
            start_time: start,
        }
    }

    fn previous(saved_at: u64, entries: usize) -> ResumeState {
        ResumeState {
            saved_at,
            entries: vec![Default::default(); entries],
            ..Default::default()
        }
    }

    #[test]
    fn pre_restart_snapshot_is_protected_only_during_the_restore_window() {
        let srv = server(1_000);
        assert!(protects_pre_restart_snapshot(
            Some(&previous(900, 2)),
            &srv,
            1_100
        ));
        // After the window closes, saves resume.
        assert!(!protects_pre_restart_snapshot(
            Some(&previous(900, 2)),
            &srv,
            1_000 + resume::RESTORE_WINDOW_SECS + 1
        ));
        // A snapshot from this server's lifetime is fair game.
        assert!(!protects_pre_restart_snapshot(
            Some(&previous(1_050, 2)),
            &srv,
            1_100
        ));
        // Once restore has run, saves go through.
        let restored = ResumeState {
            restored_at: Some(1_010),
            ..previous(900, 2)
        };
        assert!(!protects_pre_restart_snapshot(Some(&restored), &srv, 1_100));
        // Nothing worth protecting.
        assert!(!protects_pre_restart_snapshot(
            Some(&previous(900, 0)),
            &srv,
            1_100
        ));
        assert!(!protects_pre_restart_snapshot(None, &srv, 1_100));
    }
}
