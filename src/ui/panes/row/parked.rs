//! A parked session in the search results: `◌ name  repo 2d`, and on the
//! cursor row the next step from its handoff.

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::body::snippet_row;
use super::ctx::RowCtx;
use super::status::ROW_REPO_MAX_WIDTH;
use crate::park::Parked;
use crate::search::Snippet;
use crate::ui::colors::ColorTheme;
use crate::ui::text::{age_label, display_width, truncate_to_width};

/// Icon in front of a parked session.
pub(crate) const PARKED_ICON: &str = "◌";

/// Marker in front of the next step.
const NEXT_MARKER: &str = "  ↳ ";

pub(in crate::ui::panes) fn render_parked_lines(
    parked: &Parked,
    snippet: Option<&Snippet>,
    selected: bool,
    width: usize,
    theme: &ColorTheme,
    now: u64,
) -> Vec<Line<'static>> {
    let bg = selected.then_some(theme.selection_bg);
    let ctx = RowCtx {
        marker_char: " ",
        marker_style: bg.map_or_else(Style::default, |c| Style::default().bg(c)),
        inner_width: width.saturating_sub(2),
        theme,
        bg,
        active: false,
        unread: false,
    };
    let muted = ctx.apply_bg(Style::default().fg(theme.text_muted));

    let mut right = String::new();
    if !parked.repo.is_empty() {
        right.push(' ');
        right.push_str(&truncate_to_width(&parked.repo, ROW_REPO_MAX_WIDTH));
    }
    if parked.parked_at > 0 {
        right.push(' ');
        right.push_str(&age_label(now.saturating_sub(parked.parked_at)));
    }
    let icon_width = display_width(PARKED_ICON);
    let mut label_budget = ctx
        .inner_width
        .saturating_sub(icon_width + 1 + display_width(&right));
    // On a very narrow row the right side gives way to the name.
    if label_budget < 6 {
        right.clear();
        label_budget = ctx.inner_width.saturating_sub(icon_width + 1);
    }
    let label = truncate_to_width(parked.name(), label_budget);
    let left_width = icon_width + 1 + display_width(&label);
    let left = vec![
        Span::styled(PARKED_ICON, muted),
        Span::styled(
            format!(" {label}"),
            ctx.apply_bg(Style::default().fg(theme.text_active)),
        ),
    ];
    let right_width = display_width(&right);
    let right = if right.is_empty() {
        Vec::new()
    } else {
        vec![Span::styled(right, muted)]
    };
    let mut out = vec![ctx.row_line_split(left, left_width, right, right_width)];

    if let Some(snippet) = snippet {
        out.push(snippet_row(snippet, &ctx));
    }
    let next = parked
        .next_step()
        .filter(|next| snippet.is_none_or(|s| !s.text.contains(next)));
    if selected && let Some(next) = next {
        let text = truncate_to_width(&format!("{NEXT_MARKER}{next}"), ctx.inner_width);
        let text_width = display_width(&text);
        out.push(ctx.row_line(vec![Span::styled(text, muted)], text_width));
    }
    out
}
