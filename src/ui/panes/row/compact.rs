use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::RowExtras;
use super::ctx::RowCtx;
use super::status::{
    PIN_GLYPH, ROW_REPO_MAX_WIDTH, row_icon, time_label, time_label_color, time_label_room,
};
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
    let (icon, icon_color) = row_icon(pane, spinner_frame, icons, theme);

    let repo = extras
        .repo
        .map(|r| truncate_to_width(r, ROW_REPO_MAX_WIDTH))
        .unwrap_or_default();
    let label = label_text(pane, extras);
    // Icon, pin, the gap before the name, the repository and the gap
    // before the time.
    let fixed_width = display_width(icon)
        + if pane.pinned {
            1 + display_width(PIN_GLYPH)
        } else {
            0
        }
        + 1
        + if repo.is_empty() {
            0
        } else {
            1 + display_width(&repo)
        }
        + 1;
    let time = time_label(
        pane,
        extras.last_active,
        now,
        time_label_room(ctx.inner_width, fixed_width, display_width(&label)),
    );
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
        let fg = time_label_color(pane, theme, muted);
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
    let label = truncate_to_width(&label, label_budget);
    let label_fg = if extras.stale {
        theme.stale
    } else {
        theme.text_active
    };
    left_width += 1 + display_width(&label);
    let mut label_style = Style::default().fg(label_fg);
    if ctx.unread {
        label_style = label_style.add_modifier(Modifier::BOLD);
    }
    left_spans.push(Span::styled(format!(" {label}"), ctx.apply_bg(label_style)));

    ctx.row_line_split(left_spans, left_width, right_spans, right_width)
}
