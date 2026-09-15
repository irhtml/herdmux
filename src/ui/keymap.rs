use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Paragraph},
};

use crate::state::{AppState, PopupState};

const KEYMAP: &str = "Navigation\n\
j / Down / Ctrl+n: Move down\n\
k / Up / Ctrl+p: Move up\n\
Move past list edges to focus the filter or bottom panel.\n\
Enter: Focus selected agent pane\n\
Esc: Return to agent list\n\
\n\
Filters and panels\n\
Tab: Cycle status filter\n\
h/l or Left/Right: Change status (filter focused)\n\
r: Choose repository (filter focused)\n\
s: Toggle current/all sessions\n\
Shift+Tab: Switch Activity/Git\n\
\n\
Agent list\n\
n: Spawn worktree and agent\n\
x: Remove spawned pane/worktree\n\
\n\
Dialogs\n\
Esc: Cancel/close\n\
Repository: j/k or arrows, Enter to select\n\
Spawn: Tab/Shift+Tab or Up/Down to select field; Left/Right to cycle; Enter to spawn\n\
Remove: y/Enter removes window and worktree; c closes window only; n cancels\n\
\n\
Keymap\n\
?: Open/close keymap\n\
j/k, arrows, Ctrl+n/p or mouse wheel: Scroll";

pub(super) fn draw(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keymap ")
        .title_bottom(" ? / Esc: close ")
        .border_style(Style::default().fg(state.theme.accent));
    let inner = block.inner(area);
    let lines: Vec<String> = KEYMAP
        .lines()
        .flat_map(|line| {
            if line.is_empty() {
                vec![String::new()]
            } else {
                super::text::wrap_text(line, inner.width as usize, usize::MAX)
            }
        })
        .collect();
    let max_scroll = lines
        .len()
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    let paragraph =
        Paragraph::new(lines.join("\n")).style(Style::default().fg(state.theme.text_active));
    if let PopupState::Keymap { scroll } = &mut state.popup {
        *scroll = (*scroll).min(max_scroll);
        frame.render_widget(paragraph.scroll((*scroll, 0)).block(block), area);
    }
}
