use indexmap::IndexMap;

use crate::tmux;

pub(crate) fn cmd_toggle(args: &[String]) -> i32 {
    let mut create_only = false;
    let mut positional = Vec::new();

    for arg in args {
        if arg == "--create-only" {
            create_only = true;
        } else {
            positional.push(arg.as_str());
        }
    }

    let window_id = match positional.first() {
        Some(id) => *id,
        None => return 0,
    };
    let pane_path = positional.get(1).copied().unwrap_or("~");

    let layout = tmux::run_tmux(&["list-panes", "-t", window_id, "-F", &layout_format()])
        .unwrap_or_default();

    if let Some(sidebar_pane) = sidebar_pane_ids(&layout).first() {
        if !create_only {
            let _ = tmux::run_tmux(&["kill-pane", "-t", sidebar_pane]);
        }
        return 0;
    }

    open_sidebars(&plan_sidebar_splits(&layout, Some(pane_path)));
    0
}

/// Close every sidebar when any is open, otherwise open one in every
/// window. Both directions batch their tmux commands into a few client
/// invocations: one per command per window took seconds with dozens of
/// windows.
pub(crate) fn cmd_toggle_all(_args: &[String]) -> i32 {
    let list_all = || tmux::run_tmux(&["list-panes", "-a", "-F", &layout_format()]);
    let layout = list_all().unwrap_or_default();
    let mut sidebars = sidebar_pane_ids(&layout);

    if sidebars.is_empty() {
        open_sidebars(&plan_sidebar_splits(&layout, None));
        return 0;
    }

    // A sidebar that closed after the listing fails its `kill-pane` and
    // stops the batch, so re-list and retry while that makes progress.
    loop {
        let commands: Vec<Vec<&str>> = sidebars
            .iter()
            .map(|pane| vec!["kill-pane", "-t", pane.as_str()])
            .collect();
        if tmux::run_tmux_batch(&commands).0 {
            break;
        }
        let remaining = list_all()
            .map(|out| sidebar_pane_ids(&out))
            .unwrap_or_default();
        if remaining.is_empty() || remaining.len() >= sidebars.len() {
            break;
        }
        sidebars = remaining;
    }

    0
}

/// `list-panes -F` format with everything needed to find and plan
/// sidebars, one line per pane. The `@sidebar_*` options resolve per
/// pane, so window and session overrides of the global value apply.
fn layout_format() -> String {
    [
        "window_id",
        "window_width",
        "pane_left",
        "pane_width",
        "pane_id",
        tmux::PANE_ROLE,
        tmux::SIDEBAR_WIDTH,
        tmux::SIDEBAR_POSITION,
        "pane_current_path",
    ]
    .map(|field| format!("#{{q:{field}}}"))
    .join("|")
}

/// One pane line of [`layout_format`].
#[derive(Debug)]
struct LayoutPane {
    window_id: String,
    window_width: u32,
    geometry: PaneGeometry,
    is_sidebar: bool,
    width_setting: String,
    position: SidebarPosition,
    path: String,
}

/// Parse one [`layout_format`] line. Returns `None` for malformed lines so
/// callers can simply skip them.
fn parse_layout_line(line: &str) -> Option<LayoutPane> {
    let fields = tmux::split_tmux_fields(line, '|');
    let [
        window_id,
        window_width,
        left,
        width,
        pane_id,
        role,
        width_setting,
        position,
        path,
    ] = fields.as_slice()
    else {
        return None;
    };
    Some(LayoutPane {
        window_id: window_id.clone(),
        window_width: window_width.parse().unwrap_or(0),
        geometry: PaneGeometry {
            left: left.parse().ok()?,
            width: width.parse().ok()?,
            pane_id: pane_id.clone(),
        },
        is_sidebar: role == "sidebar",
        width_setting: width_setting.clone(),
        position: SidebarPosition::from_setting(position),
        path: path.clone(),
    })
}

fn sidebar_pane_ids(layout: &str) -> Vec<String> {
    layout
        .lines()
        .filter_map(parse_layout_line)
        .filter(|pane| pane.is_sidebar)
        .map(|pane| pane.geometry.pane_id)
        .collect()
}

/// What `split-window` needs to open a sidebar in one window.
#[derive(Debug, Eq, PartialEq)]
struct SidebarSplit {
    target_pane: String,
    flags: &'static str,
    width: String,
    /// Start directory for `-c`, with `#` doubled: tmux expands formats in
    /// `-c`, so a directory named `#(cmd)` would otherwise run `cmd`.
    path: String,
}

impl SidebarSplit {
    /// `-d` keeps the window's active pane, so focus never moves and the
    /// `after-select-pane` hook does not fire once per window.
    fn command<'a>(&'a self, bin: &'a str) -> Vec<&'a str> {
        vec![
            "split-window",
            "-d",
            self.flags,
            "-l",
            &self.width,
            "-t",
            &self.target_pane,
            "-c",
            &self.path,
            "-P",
            "-F",
            "#{pane_id}",
            bin,
        ]
    }
}

/// Plan one split for each window in `layout` that has no sidebar yet, in
/// listing order. `path` sets the sidebar's start directory; without it
/// the window's first pane's directory is used.
fn plan_sidebar_splits(layout: &str, path: Option<&str>) -> Vec<SidebarSplit> {
    let mut windows: IndexMap<String, Vec<LayoutPane>> = IndexMap::new();
    for pane in layout.lines().filter_map(parse_layout_line) {
        windows
            .entry(pane.window_id.clone())
            .or_default()
            .push(pane);
    }

    windows
        .values()
        .filter(|panes| !panes.iter().any(|pane| pane.is_sidebar))
        .filter_map(|panes| {
            let first = panes.first()?;
            let target_pane =
                target_pane_for_position(panes.iter().map(|pane| &pane.geometry), first.position)?;
            Some(SidebarSplit {
                target_pane,
                flags: split_window_flags(first.position),
                width: resolve_sidebar_width(&first.width_setting, first.window_width),
                path: escape_tmux_format(path.unwrap_or(&first.path)),
            })
        })
        .collect()
}

/// Make `text` expand to itself in a tmux format: `##` is a literal `#`.
fn escape_tmux_format(text: &str) -> String {
    text.replace('#', "##")
}

/// Turn the `@sidebar_width` setting into a `split-window -l` value. A
/// percentage becomes that share of `window_width` (at least one column);
/// any other value passes through, and an unset one means 30 columns.
fn resolve_sidebar_width(setting: &str, window_width: u32) -> String {
    if setting.is_empty() {
        return "30".to_string();
    }
    let Some(pct) = setting.strip_suffix('%') else {
        return setting.to_string();
    };
    let pct: u32 = pct.parse().unwrap_or(15);
    if window_width > 0 && pct > 0 {
        (window_width * pct / 100).max(1).to_string()
    } else {
        setting.to_string()
    }
}

/// Open the planned sidebars with one `split-window` batch and one
/// `set-option` batch.
fn open_sidebars(splits: &[SidebarSplit]) {
    let bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "herdmux".to_string());

    let created = split_all(splits, &bin, tmux::run_tmux_batch);

    // `-q` keeps a pane that already exited from stopping the batch.
    let roles: Vec<Vec<&str>> = created
        .iter()
        .map(|pane| {
            vec![
                "set-option",
                "-q",
                "-p",
                "-t",
                pane,
                tmux::PANE_ROLE,
                "sidebar",
            ]
        })
        .collect();
    tmux::run_tmux_batch(&roles);
}

/// Run `splits` through `run` as batches and return the created pane ids.
/// tmux stops a batch at the first failing split (e.g. "no space for new
/// pane"), and every split before it printed one pane id, so the line count
/// locates the failure: skip that window and batch the rest.
fn split_all(
    splits: &[SidebarSplit],
    bin: &str,
    mut run: impl FnMut(&[Vec<&str>]) -> (bool, String),
) -> Vec<String> {
    let mut created = Vec::new();
    let mut rest = splits;
    while !rest.is_empty() {
        let commands: Vec<Vec<&str>> = rest.iter().map(|split| split.command(bin)).collect();
        let (ok, stdout) = run(&commands);
        let before = created.len();
        created.extend(
            stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(String::from),
        );
        if ok {
            break;
        }
        rest = rest.get(created.len() - before + 1..).unwrap_or_default();
    }
    created
}

/// Which side of the window the sidebar pane is created on, driven by
/// the `@sidebar_position` tmux option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidebarPosition {
    Left,
    Right,
}

impl SidebarPosition {
    /// Parse the raw `@sidebar_position` option value. Only an explicit
    /// (case-insensitive, whitespace-tolerant) `right` selects the right
    /// side; everything else — including unset, empty, or invalid values
    /// — falls back to the historical default of `left`, so a typo never
    /// moves the sidebar somewhere unexpected.
    fn from_setting(setting: &str) -> Self {
        if setting.trim().eq_ignore_ascii_case("right") {
            Self::Right
        } else {
            Self::Left
        }
    }
}

/// Horizontal placement of one pane.
#[derive(Debug, Eq, PartialEq)]
struct PaneGeometry {
    left: u32,
    width: u32,
    pane_id: String,
}

/// Pick the pane the sidebar splits from: the leftmost pane for a left
/// sidebar, or the pane with the largest right edge (`left + width`) for
/// a right sidebar, so the new pane always lands at the window's outer
/// edge. Returns `None` when `panes` is empty.
fn target_pane_for_position<'a>(
    panes: impl Iterator<Item = &'a PaneGeometry>,
    position: SidebarPosition,
) -> Option<String> {
    match position {
        SidebarPosition::Left => panes.min_by_key(|pane| pane.left),
        SidebarPosition::Right => panes.max_by_key(|pane| pane.left.saturating_add(pane.width)),
    }
    .map(|pane| pane.pane_id.clone())
}

/// `split-window` flags for each placement: `-hfb` inserts the new pane
/// before the target (left of it), `-hf` after it (right of it). Both
/// `f` variants span the full window height.
fn split_window_flags(position: SidebarPosition) -> &'static str {
    match position {
        SidebarPosition::Left => "-hfb",
        SidebarPosition::Right => "-hf",
    }
}

/// Decide whether `cmd_auto_close` should kill the window, given the raw
/// outputs of the tmux queries it performs. Extracted as a pure function
/// so the guard logic is directly unit-testable without a running tmux
/// server.
///
/// - `list_panes_output`: `Some(stdout)` from `list-panes -F <pane role format>`,
///   or `None` if the tmux call failed.
/// - `session_windows`: parsed value of `#{session_windows}`, or `None`
///   if the tmux call failed or the value was unparseable.
/// - `session_attached`: parsed value of `#{session_attached}`, or `None`
///   if the tmux call failed or the value was unparseable.
fn should_kill_window(
    list_panes_output: Option<&str>,
    session_windows: Option<u32>,
    session_attached: Option<u32>,
) -> bool {
    // `list-panes` failed or returned nothing: the window is either gone
    // already or tmux is too busy to answer. Do NOT treat "no output"
    // as "no non-sidebar panes" — that would let us kill a live window
    // whose query happened to race with another tmux command.
    let Some(output) = list_panes_output else {
        return false;
    };
    if output.trim().is_empty() {
        return false;
    }

    let non_sidebar = output.lines().filter(|line| *line != "sidebar").count();
    if non_sidebar != 0 {
        return false;
    }

    let Some(windows) = session_windows else {
        return false;
    };

    // Last window in the session: killing it destroys the session and
    // drops every attached client. One attached client is fine — that
    // matches normal tmux `exit` behaviour on the last pane. Two or
    // more means a shared session (e.g. several terminal tabs attached
    // to `main`) where we cannot tell which clients are "wanted", so
    // preserve the sidebar instead. A missing `session_attached` errs
    // on the side of preservation.
    match windows {
        0 => false,
        1 => matches!(session_attached, Some(n) if n <= 1),
        _ => true,
    }
}

pub(crate) fn cmd_auto_close(args: &[String]) -> i32 {
    let window_id = match args.first() {
        Some(id) => id.as_str(),
        None => return 0,
    };

    let pane_role_format = format!("#{{{}}}", tmux::PANE_ROLE);
    let list_panes_output =
        tmux::run_tmux(&["list-panes", "-t", window_id, "-F", &pane_role_format]);

    let session_windows = tmux::run_tmux(&[
        "display-message",
        "-t",
        window_id,
        "-p",
        "#{session_windows}",
    ])
    .and_then(|s| s.trim().parse().ok());

    let session_attached = tmux::run_tmux(&[
        "display-message",
        "-t",
        window_id,
        "-p",
        "#{session_attached}",
    ])
    .and_then(|s| s.trim().parse().ok());

    if should_kill_window(
        list_panes_output.as_deref(),
        session_windows,
        session_attached,
    ) {
        let _ = tmux::run_tmux(&["kill-window", "-t", window_id]);
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── layout parsing and planning ─────────────────────────────────

    /// One [`layout_format`] line, with `path` already `#{q:}`-escaped.
    fn line(window: &str, left: u32, width: u32, pane: &str, role: &str, path: &str) -> String {
        format!("{window}|200|{left}|{width}|{pane}|{role}|15%|left|{path}")
    }

    #[test]
    fn sidebar_pane_ids_lists_sidebars_in_every_window() {
        let layout = [
            line("@1", 0, 30, "%1", "sidebar", "/tmp"),
            line("@1", 31, 169, "%2", "", "/tmp"),
            line("@2", 0, 200, "%3", "", "/tmp"),
            line("@3", 0, 30, "%4", "sidebar", "/tmp"),
        ]
        .join("\n");
        assert_eq!(sidebar_pane_ids(&layout), vec!["%1", "%4"]);
    }

    #[test]
    fn parse_layout_line_unescapes_path_and_skips_malformed_lines() {
        let pane = parse_layout_line(&line("@1", 4, 80, "%2", "", "/Users/me/My\\ Project"))
            .expect("valid line");
        assert_eq!(pane.path, "/Users/me/My Project");
        assert_eq!(pane.geometry.left, 4);
        assert!(!pane.is_sidebar);

        assert!(parse_layout_line("bad-line").is_none());
        assert!(
            parse_layout_line(&line("@1", 0, 80, "%2", "", "/tmp").replace("|80|", "|x|"))
                .is_none()
        );
    }

    #[test]
    fn plan_skips_windows_that_have_a_sidebar() {
        let layout = [
            line("@1", 0, 30, "%1", "sidebar", "/a"),
            line("@1", 31, 169, "%2", "", "/a"),
            line("@2", 100, 100, "%4", "", "/b/right"),
            line("@2", 0, 99, "%3", "", "/b/left"),
        ]
        .join("\n");

        assert_eq!(
            plan_sidebar_splits(&layout, None),
            vec![SidebarSplit {
                target_pane: "%3".to_string(),
                flags: "-hfb",
                width: "30".to_string(),
                path: "/b/right".to_string(),
            }]
        );
    }

    #[test]
    fn plan_uses_path_override_and_window_position() {
        let layout = [
            "@1|100|0|50|%1||20|right|/a".to_string(),
            "@1|100|51|49|%2||20|right|/a".to_string(),
        ]
        .join("\n");

        assert_eq!(
            plan_sidebar_splits(&layout, Some("/here")),
            vec![SidebarSplit {
                target_pane: "%2".to_string(),
                flags: "-hf",
                width: "20".to_string(),
                path: "/here".to_string(),
            }]
        );
    }

    #[test]
    fn plan_escapes_tmux_formats_in_the_start_directory() {
        let layout = "@1|100|0|50|%1||20|left|/a/#(touch x)#{pane_id}";
        let splits = plan_sidebar_splits(layout, None);
        assert_eq!(splits[0].path, "/a/##(touch x)##{pane_id}");
        let splits = plan_sidebar_splits(layout, Some("/b#c"));
        assert_eq!(splits[0].path, "/b##c");
    }

    #[test]
    fn resolve_sidebar_width_handles_percent_columns_and_unset() {
        assert_eq!(resolve_sidebar_width("15%", 200), "30");
        assert_eq!(resolve_sidebar_width("1%", 50), "1");
        assert_eq!(resolve_sidebar_width("15%", 0), "15%");
        assert_eq!(resolve_sidebar_width("40", 200), "40");
        assert_eq!(resolve_sidebar_width("", 200), "30");
    }

    // ─── batched splits ──────────────────────────────────────────────

    fn split(target: &str) -> SidebarSplit {
        SidebarSplit {
            target_pane: target.to_string(),
            flags: "-hfb",
            width: "30".to_string(),
            path: "/tmp".to_string(),
        }
    }

    #[test]
    fn split_all_runs_every_split_in_one_batch() {
        let splits = [split("%1"), split("%2")];
        let mut batches = Vec::new();
        let created = split_all(&splits, "herdmux", |commands| {
            batches.push(commands.to_vec().concat().join(" "));
            (true, "%10\n%11\n".to_string())
        });

        assert_eq!(created, vec!["%10", "%11"]);
        assert_eq!(
            batches,
            vec![
                "split-window -d -hfb -l 30 -t %1 -c /tmp -P -F #{pane_id} herdmux \
                 split-window -d -hfb -l 30 -t %2 -c /tmp -P -F #{pane_id} herdmux"
            ]
        );
    }

    #[test]
    fn split_all_skips_a_failed_split_and_batches_the_rest() {
        let splits = [split("%1"), split("%2"), split("%3"), split("%4")];
        let mut targets = Vec::new();
        let created = split_all(&splits, "herdmux", |commands| {
            let batch: Vec<&str> = commands.iter().map(|c| c[6]).collect();
            targets.push(batch.join(","));
            // %2 has no room; tmux prints %1's new pane and stops.
            if batch[0] == "%1" {
                (false, "%10\n".to_string())
            } else {
                (true, "%12\n%13\n".to_string())
            }
        });

        assert_eq!(created, vec!["%10", "%12", "%13"]);
        assert_eq!(targets, vec!["%1,%2,%3,%4", "%3,%4"]);
    }

    #[test]
    fn split_all_stops_when_the_last_split_fails() {
        let splits = [split("%1")];
        let mut calls = 0;
        let created = split_all(&splits, "herdmux", |_| {
            calls += 1;
            (false, String::new())
        });

        assert!(created.is_empty());
        assert_eq!(calls, 1);
    }

    // ─── sidebar placement ───────────────────────────────────────────

    #[test]
    fn sidebar_position_parses_right_only() {
        assert_eq!(
            SidebarPosition::from_setting("right"),
            SidebarPosition::Right
        );
        assert_eq!(
            SidebarPosition::from_setting(" RIGHT "),
            SidebarPosition::Right
        );
        assert_eq!(SidebarPosition::from_setting("left"), SidebarPosition::Left);
        assert_eq!(SidebarPosition::from_setting(""), SidebarPosition::Left);
        assert_eq!(
            SidebarPosition::from_setting("invalid"),
            SidebarPosition::Left
        );
    }

    fn geometry(left: u32, width: u32, pane_id: &str) -> PaneGeometry {
        PaneGeometry {
            left,
            width,
            pane_id: pane_id.to_string(),
        }
    }

    #[test]
    fn target_pane_for_left_position_uses_leftmost_pane() {
        let panes = [
            geometry(40, 80, "%3"),
            geometry(0, 20, "%1"),
            geometry(20, 20, "%2"),
        ];

        assert_eq!(
            target_pane_for_position(panes.iter(), SidebarPosition::Left),
            Some("%1".to_string())
        );
    }

    #[test]
    fn target_pane_for_right_position_uses_largest_right_edge() {
        let panes = [
            geometry(0, 20, "%1"),
            geometry(20, 20, "%2"),
            geometry(40, 80, "%3"),
        ];

        assert_eq!(
            target_pane_for_position(panes.iter(), SidebarPosition::Right),
            Some("%3".to_string())
        );
    }

    #[test]
    fn target_pane_for_position_returns_none_without_panes() {
        assert_eq!(
            target_pane_for_position([].iter(), SidebarPosition::Right),
            None
        );
    }

    #[test]
    fn split_window_flags_match_tmux_side_semantics() {
        assert_eq!(split_window_flags(SidebarPosition::Left), "-hfb");
        assert_eq!(split_window_flags(SidebarPosition::Right), "-hf");
    }

    // ─── should_kill_window ───────────────────────────────────────────

    #[test]
    fn should_kill_window_kills_when_only_sidebar_and_other_windows_exist() {
        // Classic intended path: sidebar alone in a window, session has
        // other windows to fall back on. Attached-client count is
        // irrelevant because killing this window does not end the
        // session.
        assert!(should_kill_window(Some("sidebar"), Some(2), None));
        assert!(should_kill_window(Some("sidebar"), Some(2), Some(0)));
        assert!(should_kill_window(Some("sidebar"), Some(2), Some(5)));
    }

    #[test]
    fn should_kill_window_skips_when_non_sidebar_pane_remains() {
        // Another pane with `@pane_role` explicitly set to something
        // non-sidebar (e.g. a spawn-marked pane) keeps the window alive.
        assert!(!should_kill_window(Some("sidebar\npane"), Some(5), Some(1)));
        // `@pane_role` unset renders as an empty line — that pane is
        // a regular user pane, not a sidebar, so the window must stay.
        // The real tmux output for [sidebar pane, regular pane] is
        // "sidebar\n\n" (sidebar's role, then the regular pane's empty
        // role followed by the final record separator).
        assert!(!should_kill_window(Some("sidebar\n\n"), Some(5), Some(1)));
        assert!(!should_kill_window(Some("\nsidebar\n"), Some(5), Some(1)));
    }

    #[test]
    fn should_kill_window_skips_when_list_panes_failed() {
        // `list-panes` failure must never be treated as "window is empty" —
        // that used to let a busy-tmux race kill a live window.
        assert!(!should_kill_window(None, Some(5), Some(1)));
    }

    #[test]
    fn should_kill_window_skips_when_list_panes_empty() {
        // Whitespace-only output (e.g. window already gone) must not
        // trigger a kill either.
        assert!(!should_kill_window(Some(""), Some(5), Some(1)));
        assert!(!should_kill_window(Some("   \n"), Some(5), Some(1)));
    }

    #[test]
    fn should_kill_window_kills_last_window_when_single_client_attached() {
        // One client attached to a single-window session: destroying
        // the session only detaches the same client that just kept the
        // session alive, which matches tmux's standard `exit` behaviour
        // on the last pane — the user expects the sidebar to go with it.
        assert!(should_kill_window(Some("sidebar"), Some(1), Some(1)));
    }

    #[test]
    fn should_kill_window_kills_last_window_when_detached() {
        // No clients attached: killing the session harms no one, and
        // a stranded sidebar in a detached session is pointless anyway.
        assert!(should_kill_window(Some("sidebar"), Some(1), Some(0)));
    }

    #[test]
    fn should_kill_window_preserves_last_window_when_multiple_clients_attached() {
        // Core regression guard (0dc6e99): killing the last window of
        // a session drops every attached client. With multiple terminal
        // tabs sharing a single `main` session, that manifested as every
        // tab dying at once. Keep the sidebar stranded rather than nuke
        // the session.
        assert!(!should_kill_window(Some("sidebar"), Some(1), Some(2)));
        assert!(!should_kill_window(Some("sidebar"), Some(1), Some(7)));
    }

    #[test]
    fn should_kill_window_preserves_last_window_when_attached_query_failed() {
        // Without knowing how many clients are attached we cannot prove
        // the kill is safe. Better a lingering sidebar pane than a
        // mass-disconnect.
        assert!(!should_kill_window(Some("sidebar"), Some(1), None));
    }

    #[test]
    fn should_kill_window_skips_when_session_windows_query_failed() {
        // If we cannot prove the session has other windows, err on the
        // side of preservation. Better to leave a lingering sidebar
        // pane than to destroy a live workspace.
        assert!(!should_kill_window(Some("sidebar"), None, Some(1)));
        assert!(!should_kill_window(Some("sidebar"), Some(0), Some(1)));
    }
}
