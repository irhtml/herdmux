//! Relaunch agent sessions after tmux-resurrect restores a layout.
//!
//! tmux-resurrect brings back windows, panes and their directories, but
//! agent panes come back as bare shells: it does not save pane options
//! and does not know how to resume a conversation. `resume save` runs on
//! resurrect's `post-save-layout` hook and records, per agent pane, the
//! exact command line, the allowlisted environment and the session id.
//! `resume restore` runs on `post-restore-all` and types the matching
//! resume command into each restored pane.

mod argv;
mod restore;
mod resurrect;
mod save;
mod store;

pub(crate) use argv::{ResumeCommand, build as build_command, render as render_command};
pub(crate) use restore::{Action, RESTORE_WINDOW_SECS, ServerInfo, execute, plan};
pub(crate) use resurrect::Snapshot;
pub(crate) use save::{RealSaveEnv, SaveReport, collect};
pub(crate) use store::{ResumeState, state_dir, state_path};
