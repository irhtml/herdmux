use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::RowExtras;
use super::ctx::RowCtx;
use super::status::{PIN_GLYPH, ROW_REPO_MAX_WIDTH, running_icon_for, time_label};
use crate::tmux::PaneInfo;
use crate::ui::icons::StatusIcons;
use crate::ui::text::{display_width, truncate_to_width};

/// The one-line name of an agent: its tag, its `/rename` name, the title
/// Claude Code gave the session, its first prompt, its latest prompt, or
/// the agent name, whichever comes first.
pub(crate) fn label_text(pane: &PaneInfo, extras: &RowExtras<'_>) -> String {
    let digest = extras.digest;
    let candidates = [
        pane.pane_desc.as_str(),
        pane.session_name.as_str(),
        digest.map_or("", |d| d.title.as_str()),
        digest.and_then(|d| d.goal()).unwrap_or(""),
        if pane.prompt_is_response {
            ""
        } else {
            pane.prompt.as_str()
        },
    ];
    candidates
        .into_iter()
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or(pane.agent.label())
        .to_string()
}

/// One line per agent: status icon, pin, name, and on the right the
/// repository (when the row is not under its header) and the time label.
pub(super) fn compact_row(
    pane: &PaneInfo,
    ctx: &RowCtx,
    icons: &StatusIcons,
    spinner_frame: usize,
    now: u64,
    extras: &RowExtras<'_>,
) -> Line<'static> {
    let theme = ctx.theme;
    let (icon, pulse_color) = running_icon_for(&pane.status, spinner_frame, icons);
    let icon_color =
        pulse_color.unwrap_or_else(|| theme.status_color(&pane.status, pane.attention));

    let time = time_label(pane, extras.last_active, now);
    let repo = extras
        .repo
        .map(|r| truncate_to_width(r, ROW_REPO_MAX_WIDTH))
        .unwrap_or_default();
    let muted = if extras.stale {
        theme.stale
    } else {
        theme.text_muted
    };
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    let mut right_width = 0;
    if !repo.is_empty() {
        let text = format!(" {repo}");
        right_width += display_width(&text);
        right_spans.push(Span::styled(text, ctx.apply_bg(Style::default().fg(muted))));
    }
    if !time.is_empty() {
        let text = format!(" {time}");
        right_width += display_width(&text);
        let fg = if pane.status.is_active() {
            theme.text_active
        } else {
            muted
        };
        right_spans.push(Span::styled(text, ctx.apply_bg(Style::default().fg(fg))));
    }

    let mut left_spans: Vec<Span<'static>> = vec![Span::styled(
        icon.to_string(),
        ctx.apply_bg(Style::default().fg(icon_color)),
    )];
    let mut left_width = display_width(icon);
    if pane.pinned {
        let text = format!(" {PIN_GLYPH}");
        left_width += display_width(&text);
        left_spans.push(Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(theme.accent)),
        ));
    }
    let label_budget = ctx.inner_width.saturating_sub(left_width + 1 + right_width);
    // On a very narrow row the right side gives way to the name.
    let (right_spans, right_width) = if label_budget < 6 {
        (Vec::new(), 0)
    } else {
        (right_spans, right_width)
    };
    let label_budget = ctx.inner_width.saturating_sub(left_width + 1 + right_width);
    let label = truncate_to_width(&label_text(pane, extras), label_budget);
    let label_fg = if extras.stale {
        theme.stale
    } else {
        theme.text_active
    };
    left_width += 1 + display_width(&label);
    left_spans.push(Span::styled(
        format!(" {label}"),
        ctx.apply_bg(Style::default().fg(label_fg)),
    ));

    ctx.row_line_split(left_spans, left_width, right_spans, right_width)
}
