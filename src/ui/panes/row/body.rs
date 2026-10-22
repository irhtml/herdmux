use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::ctx::RowCtx;
use crate::tmux::PaneStatus;
use crate::ui::text::{
    display_width, truncate_to_width, wait_reason_label, wrap_text, wrap_text_char,
};

pub(super) fn task_progress_row(
    task_progress: Option<&crate::activity::TaskProgress>,
    ctx: &RowCtx,
) -> Option<Line<'static>> {
    use crate::activity::TaskStatus;
    let progress = task_progress?;
    if progress.is_empty() {
        return None;
    }

    let mut icons = String::with_capacity(progress.tasks.len() * 3);
    for (_, status) in &progress.tasks {
        let ch = match status {
            TaskStatus::Completed => "✔",
            TaskStatus::InProgress => "◼",
            TaskStatus::Pending => "◻",
        };
        icons.push_str(ch);
    }
    let summary = format!(
        "  {} {}/{}",
        icons,
        progress.completed_count(),
        progress.total()
    );
    let summary_dw = display_width(&summary);
    let task_color = ctx.theme.task_progress;
    Some(ctx.row_line(
        vec![Span::styled(
            summary,
            ctx.apply_bg(Style::default().fg(task_color)),
        )],
        summary_dw,
    ))
}

pub(super) fn subagent_rows(subagents: &[String], ctx: &RowCtx) -> Vec<Line<'static>> {
    if subagents.is_empty() {
        return Vec::new();
    }
    let theme = ctx.theme;
    let subagent_color = theme.subagent;
    let tree_color = theme.text_muted;
    let last_idx = subagents.len() - 1;
    let mut out = Vec::with_capacity(subagents.len());
    for (i, sa) in subagents.iter().enumerate() {
        let connector = if i == last_idx { "└ " } else { "├ " };
        let numbered = if sa.contains('#') {
            sa.clone()
        } else {
            format!("{} #{}", sa, i + 1)
        };
        let prefix = format!("  {}", connector);
        let prefix_dw = display_width(&prefix);
        let max_sa_w = ctx.inner_width.saturating_sub(prefix_dw);
        let truncated_sa = truncate_to_width(&numbered, max_sa_w);
        let text_dw = prefix_dw + display_width(&truncated_sa);
        out.push(ctx.row_line(
            vec![
                Span::styled(prefix, ctx.apply_bg(Style::default().fg(tree_color))),
                Span::styled(
                    truncated_sa,
                    ctx.apply_bg(Style::default().fg(subagent_color)),
                ),
            ],
            text_dw,
        ));
    }
    out
}

/// What the agent is about: its `@pane_desc` tag, or the title Claude Code
/// gave the session when it has no tag. Dimmed when the agent is stale.
pub(super) fn pane_desc_row(desc: &str, ctx: &RowCtx, stale: bool) -> Option<Line<'static>> {
    if desc.is_empty() {
        return None;
    }
    let text = truncate_to_width(&format!("  {desc}"), ctx.inner_width);
    let text_dw = display_width(&text);
    let color = if stale {
        ctx.theme.stale
    } else {
        ctx.theme.pane_desc
    };
    Some(ctx.row_line(
        vec![Span::styled(text, ctx.apply_bg(Style::default().fg(color)))],
        text_dw,
    ))
}

/// Marker in front of the session's first prompt.
const GOAL_MARKER: &str = "◇ ";

/// The prompt that started the session, so the row keeps saying what it
/// was for after many follow-ups.
pub(super) fn goal_rows(goal: &str, ctx: &RowCtx) -> Vec<Line<'static>> {
    let marker_dw = display_width(GOAL_MARKER);
    let wrapped = wrap_text(goal, ctx.inner_width.saturating_sub(marker_dw), 2);
    let style = ctx.apply_bg(Style::default().fg(ctx.theme.text_muted));
    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, wl)| {
            let lead = if i == 0 { GOAL_MARKER } else { "  " };
            let text = format!("{lead}{wl}");
            let text_dw = display_width(&text);
            ctx.row_line(vec![Span::styled(text, style)], text_dw)
        })
        .collect()
}

/// Marker in front of a search snippet.
const SNIPPET_MARKER: &str = "  ↳ ";

/// The prompt a search matched, with the matched words highlighted.
pub(super) fn snippet_row(snippet: &crate::search::Snippet, ctx: &RowCtx) -> Line<'static> {
    let theme = ctx.theme;
    let base = ctx.apply_bg(Style::default().fg(theme.text_muted));
    let hit = ctx.apply_bg(
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let budget = ctx
        .inner_width
        .saturating_sub(display_width(SNIPPET_MARKER));
    let mut spans = vec![Span::styled(SNIPPET_MARKER.to_string(), base)];
    let mut used = 0;
    let mut current = String::new();
    let mut current_hit = false;
    let total_dw = display_width(&snippet.text);
    for (i, ch) in snippet.text.chars().enumerate() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        // Keep a column for the ellipsis when the text does not fit.
        if total_dw > budget && used + cw + 1 > budget {
            current.push('…');
            used += 1;
            break;
        }
        let is_hit = snippet.highlights.iter().any(|&(s, e)| i >= s && i < e);
        if is_hit != current_hit && !current.is_empty() {
            let style = if current_hit { hit } else { base };
            spans.push(Span::styled(std::mem::take(&mut current), style));
        }
        current_hit = is_hit;
        current.push(ch);
        used += cw;
    }
    if !current.is_empty() {
        let style = if current_hit && !current.ends_with('…') {
            hit
        } else {
            base
        };
        spans.push(Span::styled(current, style));
    }
    ctx.row_line(spans, display_width(SNIPPET_MARKER) + used)
}

/// Which pane started this one with `agent spawn`.
pub(super) fn spawned_by_row(spawned_by: Option<&str>, ctx: &RowCtx) -> Option<Line<'static>> {
    let parent = spawned_by?;
    let text = truncate_to_width(&format!("  spawned by {parent}"), ctx.inner_width);
    let text_dw = display_width(&text);
    Some(ctx.row_line(
        vec![Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(ctx.theme.text_muted)),
        )],
        text_dw,
    ))
}

pub(super) fn wait_reason_row(
    wait_reason: &str,
    status: &PaneStatus,
    ctx: &RowCtx,
) -> Option<Line<'static>> {
    if wait_reason.is_empty() {
        return None;
    }
    let reason = wait_reason_label(wait_reason);
    let text = format!("  {}", reason);
    let text_dw = display_width(&text);
    let reason_color = if matches!(status, PaneStatus::Error) {
        ctx.theme.status_error
    } else {
        ctx.theme.wait_reason
    };
    Some(ctx.row_line(
        vec![Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(reason_color)),
        )],
        text_dw,
    ))
}

pub(super) fn background_hint_row(ctx: &RowCtx, cmd: &str) -> Line<'static> {
    const PREFIX: &str = "  $ ";
    let room = ctx.inner_width.saturating_sub(display_width(PREFIX));
    let shown = truncate_to_width(cmd.trim(), room);
    let text = format!("{PREFIX}{shown}");
    let text_dw = display_width(&text);
    ctx.row_line(
        vec![Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(ctx.theme.status_running)),
        )],
        text_dw,
    )
}

/// The latest prompt, or the agent's reply after `▷`, wrapped to at most
/// `max_lines`. Muted when the agent is stale.
pub(super) fn prompt_rows(
    prompt: &str,
    is_response: bool,
    max_lines: usize,
    stale: bool,
    ctx: &RowCtx,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let prompt_color = if stale {
        theme.stale
    } else if ctx.active {
        theme.text_active
    } else {
        theme.text_inactive
    };
    let wrap_width = ctx.inner_width.saturating_sub(2);
    let wrapped = if is_response {
        wrap_text_char(prompt, wrap_width, max_lines)
    } else {
        wrap_text(prompt, wrap_width, max_lines)
    };

    let mut out = Vec::with_capacity(wrapped.len());
    for (li, wl) in wrapped.iter().enumerate() {
        if is_response && li == 0 {
            let arrow_color = theme.response_arrow;
            let text_dw = 2 + display_width(wl); // "▷ " width
            out.push(ctx.row_line(
                vec![
                    Span::styled(
                        "▷ ",
                        ctx.apply_bg(
                            Style::default()
                                .fg(arrow_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ),
                    Span::styled(wl.clone(), ctx.apply_bg(Style::default().fg(prompt_color))),
                ],
                text_dw,
            ));
        } else {
            let indent = "  ";
            let text = format!("{}{}", indent, wl);
            let text_dw = display_width(&text);
            out.push(ctx.row_line(
                vec![Span::styled(
                    text,
                    ctx.apply_bg(Style::default().fg(prompt_color)),
                )],
                text_dw,
            ));
        }
    }
    out
}

pub(super) fn idle_hint_row(ctx: &RowCtx) -> Line<'static> {
    let text = "  Waiting for prompt…";
    let text_dw = display_width(text);
    let idle_color = if ctx.active {
        ctx.theme.text_active
    } else {
        ctx.theme.text_inactive
    };
    ctx.row_line(
        vec![Span::styled(
            text.to_string(),
            ctx.apply_bg(Style::default().fg(idle_color)),
        )],
        text_dw,
    )
}
