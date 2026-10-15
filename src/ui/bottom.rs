mod activity;
mod git;
mod merge_requests;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::state::{AppState, BottomTab, BottomTabTarget, Focus};

use super::text::display_width;

fn render_centered(frame: &mut Frame, area: Rect, text: &str, color: Color) {
    // Vertically center: pad with empty lines above
    let top_pad = area.height.saturating_sub(1) / 2;
    let mut lines: Vec<Line<'_>> = Vec::new();
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
    let paragraph = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

pub fn draw_bottom(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let theme = &state.theme;
    let border_color = if state.focus_state.focus == Focus::ActivityLog {
        theme.accent
    } else {
        theme.border_inactive
    };

    let (tab_title, tab_ranges) = build_tab_title(state, area.width);
    // Title text starts after "╭ ".
    let title_x = area.x + 2;
    state.layout.bottom_tab_targets = tab_ranges
        .into_iter()
        .map(|(start, end, tab)| BottomTabTarget {
            start: title_x + start,
            end: title_x + end,
            tab,
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let title_spans = tab_title.spans;
    let title_dw = title_spans
        .iter()
        .map(|span| display_width(&span.content))
        .sum::<usize>();
    let top_fill_len = (area.width as usize).saturating_sub(title_dw + 4);
    let mut top_line_spans = vec![Span::styled("╭ ", Style::default().fg(border_color))];
    top_line_spans.extend(title_spans);
    top_line_spans.push(Span::styled(
        format!(" {}╮", "─".repeat(top_fill_len)),
        Style::default().fg(border_color),
    ));
    let top_line = Line::from(top_line_spans);
    let top_rect = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(Paragraph::new(top_line), top_rect);

    let bottom_line = Line::from(Span::styled(
        format!("╰{}╯", "─".repeat((area.width as usize).saturating_sub(2))),
        Style::default().fg(border_color),
    ));
    let bottom_rect = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    );
    frame.render_widget(Paragraph::new(bottom_line), bottom_rect);

    match state.bottom_tab {
        BottomTab::Activity => activity::draw_activity_content(frame, state, inner),
        BottomTab::GitStatus => git::draw_git_content(frame, state, inner),
        BottomTab::MergeRequests => {
            merge_requests::draw_merge_requests_content(frame, state, inner)
        }
    }
}

/// Tab label sets from widest to narrowest, with the separator between
/// labels. The first set whose title fits the panel is drawn, so a narrow
/// sidebar keeps every tab and the top-right corner visible.
const TAB_TITLE_VARIANTS: [([&str; 3], &str); 3] = [
    (["Activity", "Git", "MRs"], " \u{2502} "),
    (["Act", "Git", "MRs"], " \u{2502} "),
    (["Act", "Git", "MRs"], "\u{2502}"),
];

const TABS: [BottomTab; 3] = [
    BottomTab::Activity,
    BottomTab::GitStatus,
    BottomTab::MergeRequests,
];

/// Build the tab header title for a panel `width` columns wide. Also
/// returns each tab's clickable column range relative to the title start;
/// with spaced separators a range includes the space on either side.
fn build_tab_title(state: &AppState, width: u16) -> (Line<'static>, Vec<(u16, u16, BottomTab)>) {
    let theme = &state.theme;
    // "╭ " + title + " ╮"
    let available = (width as usize).saturating_sub(4);
    let (labels, separator) = TAB_TITLE_VARIANTS
        .iter()
        .find(|(labels, sep)| {
            labels.iter().map(|l| display_width(l)).sum::<usize>()
                + display_width(sep) * (labels.len() - 1)
                <= available
        })
        .unwrap_or(&TAB_TITLE_VARIANTS[TAB_TITLE_VARIANTS.len() - 1]);
    let pad = u16::from(separator.starts_with(' '));
    let sep_style = Style::default().fg(theme.border_inactive);

    let mut spans = Vec::new();
    let mut ranges = Vec::new();
    let mut x: u16 = 0;
    for (i, (label, tab)) in labels.iter().zip(TABS).enumerate() {
        if i > 0 {
            spans.push(Span::styled(*separator, sep_style));
            x += display_width(separator) as u16;
        }
        let style = if state.bottom_tab == tab {
            Style::default().fg(theme.accent)
        } else {
            Style::default().fg(theme.text_muted)
        };
        let w = display_width(label) as u16;
        spans.push(Span::styled(*label, style));
        ranges.push((x.saturating_sub(pad), x + w + pad, tab));
        x += w;
    }
    (Line::from(spans), ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text::truncate_to_width;

    fn title_text(width: u16) -> (String, Vec<(u16, u16, BottomTab)>) {
        let state = AppState::new("%99".into());
        let (line, ranges) = build_tab_title(&state, width);
        let text = line.spans.iter().map(|s| s.content.as_ref()).collect();
        (text, ranges)
    }

    #[test]
    fn tab_title_shortens_to_fit_narrow_panels() {
        assert_eq!(title_text(24).0, "Activity │ Git │ MRs");
        assert_eq!(title_text(23).0, "Act │ Git │ MRs");
        assert_eq!(title_text(18).0, "Act│Git│MRs");
        // Narrower than every variant: the tightest one is used.
        assert_eq!(title_text(10).0, "Act│Git│MRs");
    }

    #[test]
    fn tab_click_ranges_follow_the_drawn_labels() {
        use BottomTab::*;
        assert_eq!(
            title_text(30).1,
            vec![
                (0, 9, Activity),
                (10, 15, GitStatus),
                (16, 21, MergeRequests)
            ]
        );
        assert_eq!(
            title_text(18).1,
            vec![(0, 3, Activity), (4, 7, GitStatus), (8, 11, MergeRequests)]
        );
    }

    #[test]
    fn truncate_to_width_short() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
    }

    #[test]
    fn truncate_to_width_exact() {
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn truncate_to_width_truncated() {
        let result = truncate_to_width("hello world", 8);
        assert!(result.ends_with('…'));
        assert!(result.len() <= 10); // 7 chars + ellipsis in bytes
    }
}
