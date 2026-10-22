use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::RowExtras;
use super::ctx::RowCtx;
use crate::tmux::PaneStatus;
use crate::ui::icons::StatusIcons;
use crate::ui::text::{age_label, display_width, elapsed_label, truncate_to_width};

/// Glyph in front of a pinned agent's name.
pub(super) const PIN_GLYPH: &str = "★";

/// Longest repository name shown on a row that is not under its header.
pub(super) const ROW_REPO_MAX_WIDTH: usize = 16;

/// Right-hand time label of an agent row: the running time of the current
/// turn while the agent works, otherwise how long ago it was last active.
pub(super) fn time_label(
    pane: &crate::tmux::PaneInfo,
    last_active: Option<u64>,
    now: u64,
) -> String {
    match last_active {
        Some(at) if !pane.status.is_active() && at > 0 => age_label(now.saturating_sub(at)),
        _ => elapsed_label(pane.started_at, now),
    }
}

pub(super) fn status_row(
    pane: &crate::tmux::PaneInfo,
    ctx: &RowCtx,
    icons: &StatusIcons,
    spinner_frame: usize,
    now: u64,
    extras: &RowExtras<'_>,
) -> Line<'static> {
    use crate::tmux::PermissionMode;
    let theme = ctx.theme;

    let (icon, pulse_color) = running_icon_for(&pane.status, spinner_frame, icons);
    let icon_color =
        pulse_color.unwrap_or_else(|| theme.status_color(&pane.status, pane.attention));
    let title_raw: &str = if pane.session_name.is_empty() {
        pane.agent.label()
    } else {
        &pane.session_name
    };
    let badge = pane.permission_mode.badge();
    let elapsed = time_label(pane, extras.last_active, now);

    let title_fg = theme.agent_color(&pane.agent);
    let elapsed_fg = if pane.status.is_active() {
        theme.text_active
    } else {
        theme.text_muted
    };

    let pin = if pane.pinned {
        format!(" {PIN_GLYPH}")
    } else {
        String::new()
    };
    let repo = extras
        .repo
        .map(|r| format!(" · {}", truncate_to_width(r, ROW_REPO_MAX_WIDTH)))
        .unwrap_or_default();
    let badge_extra = if badge.is_empty() { 0 } else { 1 };
    let fixed_width = display_width(icon)
        + display_width(&pin)
        + 1
        + badge_extra
        + display_width(badge)
        + display_width(&repo);
    // User-supplied session names (set via `/rename`) can be arbitrarily
    // long; cap the title to the space left after reserving room for the
    // icon, badge, and elapsed label so they stay visible instead of
    // being pushed off-screen.
    let elapsed_width = display_width(&elapsed);
    let elapsed_gap = usize::from(elapsed_width > 0);
    let title_budget = ctx
        .inner_width
        .saturating_sub(fixed_width + elapsed_gap + elapsed_width);
    let title = truncate_to_width(title_raw, title_budget);

    let left_width = fixed_width + display_width(&title);
    let available_for_elapsed = ctx.inner_width.saturating_sub(left_width);
    let elapsed = truncate_to_width(&elapsed, available_for_elapsed);
    let elapsed_width = display_width(&elapsed);

    let mut left_spans: Vec<Span<'static>> = Vec::with_capacity(5);
    left_spans.push(Span::styled(
        icon.to_string(),
        ctx.apply_bg(Style::default().fg(icon_color)),
    ));
    if !pin.is_empty() {
        left_spans.push(Span::styled(
            pin,
            ctx.apply_bg(Style::default().fg(theme.accent)),
        ));
    }
    left_spans.push(Span::styled(
        format!(" {}", title),
        ctx.apply_bg(Style::default().fg(title_fg)),
    ));
    if !badge.is_empty() {
        let badge_color = match pane.permission_mode {
            PermissionMode::BypassPermissions => theme.badge_danger,
            PermissionMode::Auto => theme.badge_auto,
            PermissionMode::DontAsk => theme.badge_auto,
            PermissionMode::Plan => theme.badge_plan,
            PermissionMode::AcceptEdits => theme.badge_auto,
            PermissionMode::Defer => theme.badge_auto,
            PermissionMode::Default => theme.text_muted,
        };
        left_spans.push(Span::styled(
            format!(" {}", badge),
            ctx.apply_bg(Style::default().fg(badge_color)),
        ));
    }
    if !repo.is_empty() {
        left_spans.push(Span::styled(
            repo,
            ctx.apply_bg(Style::default().fg(theme.text_muted)),
        ));
    }

    let right_spans = vec![Span::styled(
        elapsed,
        ctx.apply_bg(Style::default().fg(elapsed_fg)),
    )];

    ctx.row_line_split(left_spans, left_width, right_spans, elapsed_width)
}

pub(super) fn running_icon_for<'a>(
    status: &PaneStatus,
    spinner_frame: usize,
    icons: &'a StatusIcons,
) -> (&'a str, Option<Color>) {
    use crate::SPINNER_PULSE;

    match status {
        PaneStatus::Running => {
            let color_idx = SPINNER_PULSE[spinner_frame % SPINNER_PULSE.len()];
            (icons.status_icon(status), Some(Color::Indexed(color_idx)))
        }
        _ => (icons.status_icon(status), None),
    }
}
