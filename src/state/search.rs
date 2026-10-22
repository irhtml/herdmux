use std::cmp::Reverse;

use super::{AppMode, AppState, Focus};
use crate::search::{self, Candidate, Snippet, weight};

/// One agent that matches the search, best first in [`SearchState::hits`].
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub pane_id: String,
    pub score: u32,
    /// Matched prompt to show under the row, when the match is not in
    /// anything the row already shows.
    pub snippet: Option<Snippet>,
}

/// The `/` search line. While it is open the agent list shows every agent
/// in every tmux session, ignoring the scope, status and repo filters.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// The search line is open and takes typed keys.
    pub open: bool,
    pub query: String,
    /// Cursor within `hits`. Kept apart from the shared sidebar cursor so
    /// searching in one sidebar does not move the others.
    pub selected: usize,
    /// Matches for `query`, best first. Rebuilt by `rebuild_row_targets`.
    pub hits: Vec<SearchHit>,
    /// The agent under the shared cursor when the search opened. Esc puts
    /// the cursor back on it.
    pub return_to: Option<String>,
}

impl AppState {
    /// Open the search line (the `/` key).
    pub fn open_search(&mut self) {
        if self.search.open {
            return;
        }
        self.begin_search();
        // Search covers every tmux session, so drop the session scope for
        // this snapshot.
        self.refresh();
    }

    /// The state side of [`Self::open_search`], without the tmux refresh
    /// that rebuilds the rows.
    pub fn begin_search(&mut self) {
        self.search = SearchState {
            open: true,
            return_to: self.selected_pane().map(|p| p.pane_id.clone()),
            ..SearchState::default()
        };
        // The cursor and scrolling follow the results, which only happens
        // while the agent list has focus.
        self.focus_state.focus = Focus::Panes;
        self.scrolls.panes.offset = 0;
        // The old rows belong to the normal list; reading them as search
        // rows would start the cursor on an unrelated agent.
        self.layout.pane_row_targets.clear();
    }

    /// Close the search line and go back to the normal list, with the
    /// cursor where it was. The picker has nothing else to show, so it
    /// quits instead.
    pub fn close_search(&mut self) {
        let back = self.search.return_to.clone();
        self.leave_search(back);
    }

    /// Jump to the selected match and close the search line, leaving the
    /// cursor on that agent.
    pub fn confirm_search(&mut self) {
        let Some(target) = self.selected_pane().map(|p| p.pane_id.clone()) else {
            return;
        };
        self.activate_selected_pane();
        self.leave_search(Some(target));
    }

    fn leave_search(&mut self, cursor_on: Option<String>) {
        if self.mode == AppMode::Picker {
            self.quit = true;
            return;
        }
        if !self.search.open {
            return;
        }
        self.end_search();
        self.refresh();
        self.keep_cursor_on(cursor_on);
    }

    /// Reset the search and forget its rows, so the next rebuild does not
    /// read search rows as the normal list. Returns the agent the cursor
    /// was on before the search opened.
    pub(crate) fn end_search(&mut self) -> Option<String> {
        let back = self.search.return_to.take();
        self.search = SearchState::default();
        self.layout.pane_row_targets.clear();
        back
    }

    pub fn search_push_char(&mut self, c: char) {
        self.search.query.push(c);
        self.search_query_changed();
    }

    pub fn search_pop_char(&mut self) {
        self.search.query.pop();
        self.search_query_changed();
    }

    /// Delete the last word of the query (Ctrl+w).
    pub fn search_delete_word(&mut self) {
        let trimmed = self.search.query.trim_end();
        let cut = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        self.search.query.truncate(cut);
        self.search_query_changed();
    }

    pub fn search_clear_query(&mut self) {
        self.search.query.clear();
        self.search_query_changed();
    }

    /// A new query starts the cursor on the best match. Other rebuilds keep
    /// it on the agent it is on.
    fn search_query_changed(&mut self) {
        self.rebuild_row_targets();
        self.search.selected = 0;
    }

    /// Recompute `search.hits` from the current panes and digests.
    pub(crate) fn rebuild_search_hits(&mut self) {
        let terms = search::parse_query(&self.search.query);
        let mut hits: Vec<(SearchHit, Option<u64>)> = Vec::new();
        for group in &self.repo_groups {
            for (pane, git) in &group.panes {
                let digest = self.digest_for(pane);
                let labels = [
                    (pane.pane_desc.as_str(), weight::TAG),
                    (pane.session_name.as_str(), weight::SESSION_NAME),
                    (digest.map_or("", |d| d.title.as_str()), weight::TITLE),
                    (pane.window_name.as_str(), weight::WINDOW),
                    (group.name.as_str(), weight::REPO),
                    (git.branch.as_deref().unwrap_or(""), weight::BRANCH),
                    (pane.tmux_session.as_str(), weight::TMUX_SESSION),
                    (pane.path.as_str(), weight::PATH),
                ];
                let mut candidate = Candidate {
                    labels: labels.into_iter().filter(|(t, _)| !t.is_empty()).collect(),
                    texts: Vec::new(),
                };
                if !pane.prompt.is_empty() {
                    candidate.texts.push((&pane.prompt, weight::LAST_PROMPT));
                }
                if let Some(digest) = digest {
                    if let Some(goal) = digest.goal() {
                        candidate.texts.push((goal, weight::GOAL));
                    }
                    for prompt in digest.prompts.iter().rev() {
                        candidate.texts.push((prompt, weight::HISTORY));
                    }
                }
                if let Some(scored) = search::score(&terms, &candidate) {
                    hits.push((
                        SearchHit {
                            pane_id: pane.pane_id.clone(),
                            score: scored.score,
                            snippet: scored.snippet,
                        },
                        self.last_active(pane),
                    ));
                }
            }
        }
        hits.sort_by_key(|(hit, last_active)| (Reverse(hit.score), Reverse(*last_active)));
        self.search.hits = hits.into_iter().map(|(hit, _)| hit).collect();
    }
}
