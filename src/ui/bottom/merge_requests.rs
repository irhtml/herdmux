use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::state::{AppState, MergeRequestTarget};
use crate::ui::text::{display_width, truncate_to_width};

/// Indent of an MR row's second line (repo and branch).
const DETAIL_INDENT: &str = "  ";
const SEPARATOR: &str = " · ";

/// Draw the MRs tab: two lines per open MR (number and title, then repo
/// and branch), scrollable, with each visible line registered as a click
/// target for its MR's URL.
pub(super) fn draw_merge_requests_content(frame: &mut Frame, state: &mut AppState, inner: Rect) {
    let theme = &state.theme;
    let list = &state.merge_requests.list;
    if list.is_empty() {
        let text = if state.merge_requests.pending {
            "Loading…"
        } else {
            "No open MRs in this session"
        };
        super::render_centered(frame, inner, text, theme.text_muted);
        state.scrolls.merge_requests = Default::default();
        return;
    }

    let inner_w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(list.len() * 2);
    let mut urls: Vec<&str> = Vec::with_capacity(list.len() * 2);
    for mr in list {
        let label = mr.label();
        let title_w = inner_w.saturating_sub(display_width(&label) + 1);
        let title_color = if mr.draft {
            theme.text_inactive
        } else {
            theme.text_active
        };
        lines.push(Line::from(vec![
            Span::styled(
                label,
                Style::default()
                    .fg(theme.pr_link)
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Span::raw(" "),
            Span::styled(
                truncate_to_width(&mr.title, title_w),
                Style::default().fg(title_color),
            ),
        ]));

        let mut detail = vec![Span::raw(DETAIL_INDENT)];
        let mut remaining = inner_w.saturating_sub(DETAIL_INDENT.len());
        let mut push = |text: &str, style: Style, detail: &mut Vec<Span<'static>>| {
            if remaining == 0 {
                return;
            }
            let fitted = truncate_to_width(text, remaining);
            remaining = remaining.saturating_sub(display_width(&fitted));
            detail.push(Span::styled(fitted, style));
        };
        let sep_style = Style::default().fg(theme.text_inactive);
        if mr.draft {
            push(
                "draft",
                Style::default().fg(theme.text_inactive),
                &mut detail,
            );
            push(SEPARATOR, sep_style, &mut detail);
        }
        push(&mr.repo, Style::default().fg(theme.text_muted), &mut detail);
        push(SEPARATOR, sep_style, &mut detail);
        push(&mr.branch, Style::default().fg(theme.branch), &mut detail);
        lines.push(Line::from(detail));

        urls.push(&mr.url);
        urls.push(&mr.url);
    }

    let scroll = &mut state.scrolls.merge_requests;
    scroll.total_lines = lines.len();
    scroll.visible_height = inner.height as usize;
    scroll.scroll(0);
    let offset = scroll.offset;

    state.layout.merge_request_targets = urls
        .iter()
        .skip(offset)
        .take(inner.height as usize)
        .enumerate()
        .map(|(i, url)| MergeRequestTarget {
            y: inner.y + i as u16,
            url: (*url).to_string(),
        })
        .collect();

    let paragraph = Paragraph::new(lines).scroll((offset as u16, 0));
    frame.render_widget(paragraph, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge_request::{Forge, MergeRequest};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn mr(number: u64, title: &str, repo: &str, branch: &str, forge: Forge) -> MergeRequest {
        MergeRequest {
            repo: repo.into(),
            branch: branch.into(),
            forge,
            number,
            title: title.into(),
            url: format!("https://example.com/{repo}/{number}"),
            draft: false,
        }
    }

    fn render(state: &mut AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_merge_requests_content(frame, state, Rect::new(0, 0, width, height));
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut rows: Vec<String> = Vec::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            rows.push(line.trim_end().to_string());
        }
        while rows.last().is_some_and(|l| l.is_empty()) {
            rows.pop();
        }
        rows.join("\n")
    }

    fn state_with(list: Vec<MergeRequest>) -> AppState {
        let mut state = AppState::new("%99".into());
        state.apply_merge_requests(list, false);
        state
    }

    #[test]
    fn loading_before_first_result() {
        let mut state = AppState::new("%99".into());
        insta::assert_snapshot!(render(&mut state, 30, 3), @"

                   Loading…
        ");
        assert!(state.layout.merge_request_targets.is_empty());
    }

    #[test]
    fn empty_after_lookup() {
        let mut state = state_with(vec![]);
        insta::assert_snapshot!(render(&mut state, 30, 3), @"

         No open MRs in this session
        ");
    }

    #[test]
    fn rows_show_number_title_repo_and_branch() {
        let mut draft = mr(
            128,
            "Add rate limiting to the login endpoint",
            "storefront",
            "feature/ABC-128-login-rate-limit",
            Forge::GitLab,
        );
        draft.draft = true;
        let mut state = state_with(vec![
            draft,
            mr(4, "Register hook", "herdmux", "ticket-3", Forge::GitHub),
        ]);
        insta::assert_snapshot!(render(&mut state, 36, 6), @"
        !128 Add rate limiting to the login…
          draft · storefront · feature/ABC-…
        #4 Register hook
          herdmux · ticket-3
        ");
    }

    #[test]
    fn click_targets_cover_visible_lines_after_scroll() {
        let mut state = state_with(vec![
            mr(1, "One", "api", "a", Forge::GitLab),
            mr(2, "Two", "api", "b", Forge::GitLab),
            mr(3, "Three", "api", "c", Forge::GitLab),
        ]);
        render(&mut state, 30, 3);
        state.scrolls.merge_requests.scroll(2);
        let output = render(&mut state, 30, 3);
        insta::assert_snapshot!(output, @"
        !2 Two
          api · b
        !3 Three
        ");
        let targets: Vec<(u16, &str)> = state
            .layout
            .merge_request_targets
            .iter()
            .map(|t| (t.y, t.url.as_str()))
            .collect();
        assert_eq!(
            targets,
            vec![
                (0, "https://example.com/api/2"),
                (1, "https://example.com/api/2"),
                (2, "https://example.com/api/3"),
            ]
        );
    }
}
