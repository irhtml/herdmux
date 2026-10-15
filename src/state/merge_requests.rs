use crate::merge_request::MergeRequest;

use super::{AppState, BottomTab};

/// Contents of the MRs bottom tab, fed by the MR poll thread.
#[derive(Debug, Clone)]
pub struct MergeRequestsState {
    pub list: Vec<MergeRequest>,
    /// No pass has published yet, or lookups for some branches are still
    /// running, so an empty list renders as loading instead of "none".
    pub pending: bool,
}

impl MergeRequestsState {
    pub fn new() -> Self {
        Self {
            list: Vec::new(),
            pending: true,
        }
    }
}

impl Default for MergeRequestsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Click target for one rendered line of the MRs tab. Both lines of an MR
/// row point at the same URL.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeRequestTarget {
    pub y: u16,
    pub url: String,
}

impl AppState {
    pub fn apply_merge_requests(&mut self, list: Vec<MergeRequest>, pending: bool) {
        self.merge_requests.list = list;
        self.merge_requests.pending = pending;
    }

    /// Handle a left click inside the bottom panel body (below the tab
    /// header). On the MRs tab, a click on a row queues its URL for the
    /// event loop to open in the browser.
    pub fn handle_bottom_body_click(&mut self, row: u16) {
        if self.bottom_tab != BottomTab::MergeRequests {
            return;
        }
        if let Some(target) = self
            .layout
            .merge_request_targets
            .iter()
            .find(|t| t.y == row)
        {
            self.pending_open_url = Some(target.url.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(y: u16, url: &str) -> MergeRequestTarget {
        MergeRequestTarget { y, url: url.into() }
    }

    #[test]
    fn new_state_is_pending() {
        let state = MergeRequestsState::new();
        assert!(state.list.is_empty());
        assert!(state.pending);
    }

    #[test]
    fn body_click_queues_url_of_clicked_row() {
        let mut state = AppState::new("%99".into());
        state.bottom_tab = BottomTab::MergeRequests;
        state.layout.merge_request_targets =
            vec![target(20, "https://a/1"), target(21, "https://a/1")];
        state.handle_bottom_body_click(22);
        assert_eq!(state.pending_open_url, None);
        state.handle_bottom_body_click(21);
        assert_eq!(state.pending_open_url.as_deref(), Some("https://a/1"));
    }

    #[test]
    fn body_click_ignored_on_other_tabs() {
        let mut state = AppState::new("%99".into());
        state.bottom_tab = BottomTab::GitStatus;
        state.layout.merge_request_targets = vec![target(20, "https://a/1")];
        state.handle_bottom_body_click(20);
        assert_eq!(state.pending_open_url, None);
    }
}
