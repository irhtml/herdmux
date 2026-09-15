#[allow(dead_code)]
mod test_helpers;

use test_helpers::render_to_string;
use tmux_agent_sidebar::state::{AppState, PopupState};

#[test]
fn keymap_wide() {
    let mut state = AppState::new("%99".into());
    state.popup = PopupState::Keymap { scroll: 0 };
    let output = render_to_string(&mut state, 80, 32);
    insta::assert_snapshot!(output, @r"
    ┌ Keymap ──────────────────────────────────────────────────────────────────────┐
    │Navigation                                                                    │
    │j / Down / Ctrl+n: Move down                                                  │
    │k / Up / Ctrl+p: Move up                                                      │
    │Move past list edges to focus the filter or bottom panel.                     │
    │Enter: Focus selected agent pane                                              │
    │Esc: Return to agent list                                                     │
    │Filters and panels                                                            │
    │Tab: Cycle status filter                                                      │
    │h/l or Left/Right: Change status (filter focused)                             │
    │r: Choose repository (filter focused)                                         │
    │s: Toggle current/all sessions                                                │
    │Shift+Tab: Switch Activity/Git                                                │
    │Agent list                                                                    │
    │n: Spawn worktree and agent                                                   │
    │x: Remove spawned pane/worktree                                               │
    │Dialogs                                                                       │
    │Esc: Cancel/close                                                             │
    │Repository: j/k or arrows, Enter to select                                    │
    │Spawn: Tab/Shift+Tab or Up/Down to select field; Left/Right to cycle; Enter   │
    │to spawn                                                                      │
    │Remove: y/Enter removes window and worktree; c closes window only; n cancels  │
    │Keymap                                                                        │
    │?: Open/close keymap                                                          │
    │j/k, arrows, Ctrl+n/p or mouse wheel: Scroll                                  │
    └ ? / Esc: close ──────────────────────────────────────────────────────────────┘
    ");
}

#[test]
fn keymap_narrow_scrolled_to_end() {
    let mut state = AppState::new("%99".into());
    state.popup = PopupState::Keymap { scroll: u16::MAX };
    let output = render_to_string(&mut state, 30, 12);
    insta::assert_snapshot!(output, @r"
    ┌ Keymap ────────────────────┐
    │to spawn                    │
    │Remove: y/Enter removes     │
    │window and worktree; c      │
    │closes window only; n       │
    │cancels                     │
    │Keymap                      │
    │?: Open/close keymap        │
    │j/k, arrows, Ctrl+n/p or    │
    │mouse wheel: Scroll         │
    └ ? / Esc: close ────────────┘
    ");
    assert!(matches!(state.popup, PopupState::Keymap { scroll } if scroll < u16::MAX));
}
