//! Matching for the sidebar's agent search.
//!
//! A query is split into words, and an agent matches when every word
//! appears (case-insensitively) somewhere in its fields: tag, names, repo,
//! branch, or one of its prompts. Each word scores the weight of the best
//! field it was found in, so a word in the tag outranks the same word deep
//! in the prompt history.

/// Weights for the fields a word can match in.
pub mod weight {
    pub const TAG: u32 = 100;
    pub const SESSION_NAME: u32 = 90;
    pub const TITLE: u32 = 80;
    pub const WINDOW: u32 = 60;
    pub const REPO: u32 = 50;
    pub const BRANCH: u32 = 50;
    pub const GOAL: u32 = 35;
    pub const TMUX_SESSION: u32 = 30;
    pub const LAST_PROMPT: u32 = 30;
    pub const PATH: u32 = 20;
    pub const HISTORY: u32 = 20;
}

/// Characters of context kept before the first match in a snippet.
const SNIPPET_LEAD: usize = 16;
/// Longest snippet, in characters. The renderer cuts it to the row width.
const SNIPPET_MAX: usize = 200;

/// What one agent offers to the matcher.
#[derive(Debug, Default)]
pub struct Candidate<'a> {
    /// Short fields already shown on the row (tag, names, repo, branch).
    pub labels: Vec<(&'a str, u32)>,
    /// Prompts, most useful first. A match here is shown as a snippet.
    pub texts: Vec<(&'a str, u32)>,
}

/// Part of a prompt shown under a search result, with the matched words.
#[derive(Debug, Clone, PartialEq)]
pub struct Snippet {
    pub text: String,
    /// Character ranges of `text` to highlight, sorted and non-overlapping.
    pub highlights: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub score: u32,
    /// Set when a word matched only in a prompt, which the row does not
    /// show on its own.
    pub snippet: Option<Snippet>,
}

/// Split a query into lowercase words.
pub fn parse_query(query: &str) -> Vec<String> {
    query.split_whitespace().map(lowercase_chars).collect()
}

/// Lowercase each character to exactly one character, so character
/// positions in the result line up with the original text.
fn lowercase_chars(text: &str) -> String {
    text.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

/// Score `candidate` against `terms`. `None` when a word matches nowhere.
pub fn score(terms: &[String], candidate: &Candidate<'_>) -> Option<Scored> {
    if terms.is_empty() {
        return Some(Scored {
            score: 0,
            snippet: None,
        });
    }
    let labels: Vec<(String, u32)> = candidate
        .labels
        .iter()
        .map(|(text, w)| (lowercase_chars(text), *w))
        .collect();
    let texts: Vec<(String, u32)> = candidate
        .texts
        .iter()
        .map(|(text, w)| (lowercase_chars(text), *w))
        .collect();

    let mut total = 0;
    let mut needs_snippet = false;
    for term in terms {
        let in_label = labels
            .iter()
            .filter(|(text, _)| text.contains(term.as_str()))
            .map(|(_, w)| *w)
            .max();
        let in_text = texts
            .iter()
            .filter(|(text, _)| text.contains(term.as_str()))
            .map(|(_, w)| *w)
            .max();
        match (in_label, in_text) {
            (None, None) => return None,
            (Some(l), Some(t)) => total += l.max(t),
            (Some(l), None) => total += l,
            (None, Some(t)) => {
                total += t;
                needs_snippet = true;
            }
        }
    }

    let snippet = if needs_snippet {
        // The prompt that holds the most words, earliest in the list on ties.
        let best = texts
            .iter()
            .enumerate()
            .map(|(i, (text, _))| {
                let hits = terms.iter().filter(|t| text.contains(t.as_str())).count();
                (hits, i)
            })
            .filter(|(hits, _)| *hits > 0)
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        best.map(|(_, i)| make_snippet(candidate.texts[i].0, &texts[i].0, terms))
    } else {
        None
    };
    Some(Scored {
        score: total,
        snippet,
    })
}

/// Character positions where `needle` occurs in `hay` (both as char vectors).
fn find_all(hay: &[char], needle: &[char]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
    (0..=hay.len() - needle.len())
        .filter(|&i| hay[i..i + needle.len()] == *needle)
        .collect()
}

fn make_snippet(original: &str, lower: &str, terms: &[String]) -> Snippet {
    let chars: Vec<char> = original.chars().collect();
    let lower: Vec<char> = lower.chars().collect();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        let needle: Vec<char> = term.chars().collect();
        for pos in find_all(&lower, &needle) {
            ranges.push((pos, pos + needle.len()));
        }
    }
    ranges.sort_unstable();
    let first = ranges.first().map_or(0, |r| r.0);
    let start = first.saturating_sub(SNIPPET_LEAD);
    let end = (start + SNIPPET_MAX).min(chars.len());

    let lead = if start > 0 { "…" } else { "" };
    let offset = lead.chars().count();
    let mut text: String = lead.to_string();
    text.extend(&chars[start..end]);

    let mut highlights: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        if s < start || s >= end {
            continue;
        }
        let (s, e) = (s - start + offset, e.min(end) - start + offset);
        match highlights.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => highlights.push((s, e)),
        }
    }
    Snippet { text, highlights }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand<'a>(labels: &[(&'a str, u32)], texts: &[(&'a str, u32)]) -> Candidate<'a> {
        Candidate {
            labels: labels.to_vec(),
            texts: texts.to_vec(),
        }
    }

    #[test]
    fn every_word_must_match_somewhere() {
        let c = cand(
            &[
                ("Dockerfile security fix", weight::TAG),
                ("billing", weight::REPO),
            ],
            &[],
        );
        assert!(score(&parse_query("dockerfile BILLING"), &c).is_some());
        assert!(score(&parse_query("dockerfile nginx"), &c).is_none());
    }

    #[test]
    fn best_field_wins_per_word() {
        let c = cand(
            &[("php upgrade", weight::TAG)],
            &[("upgrade php to 8.3", weight::HISTORY)],
        );
        let scored = score(&parse_query("php"), &c).unwrap();
        assert_eq!(scored.score, weight::TAG);
        assert_eq!(scored.snippet, None, "visible label match needs no snippet");
    }

    #[test]
    fn history_match_returns_highlighted_snippet() {
        let c = cand(
            &[("PHP CVE research", weight::TAG)],
            &[
                ("check now", weight::LAST_PROMPT),
                ("rebuild the Dockerfile with php 8.3", weight::HISTORY),
            ],
        );
        let scored = score(&parse_query("dockerfile"), &c).unwrap();
        assert_eq!(scored.score, weight::HISTORY);
        let snippet = scored.snippet.unwrap();
        assert_eq!(snippet.text, "rebuild the Dockerfile with php 8.3");
        assert_eq!(snippet.highlights, vec![(12, 22)]);
    }

    #[test]
    fn snippet_starts_near_the_match_in_long_prompts() {
        let long = format!("{} the needle is here", "x".repeat(100));
        let c = cand(&[], &[(&long, weight::HISTORY)]);
        let snippet = score(&parse_query("needle"), &c).unwrap().snippet.unwrap();
        assert!(snippet.text.starts_with('…'));
        let (s, e) = snippet.highlights[0];
        let shown: String = snippet.text.chars().skip(s).take(e - s).collect();
        assert_eq!(shown, "needle");
    }

    #[test]
    fn snippet_prefers_the_prompt_with_most_words() {
        let c = cand(
            &[],
            &[
                ("nginx config", weight::LAST_PROMPT),
                ("nginx and php config", weight::HISTORY),
            ],
        );
        let snippet = score(&parse_query("nginx php"), &c)
            .unwrap()
            .snippet
            .unwrap();
        assert_eq!(snippet.text, "nginx and php config");
        assert_eq!(snippet.highlights, vec![(0, 5), (10, 13)]);
    }

    #[test]
    fn overlapping_highlights_merge() {
        let c = cand(&[], &[("deployment", weight::HISTORY)]);
        let snippet = score(&parse_query("deploy ploy"), &c)
            .unwrap()
            .snippet
            .unwrap();
        assert_eq!(snippet.highlights, vec![(0, 6)]);
    }

    #[test]
    fn empty_query_matches_everything_with_zero_score() {
        let c = cand(&[], &[]);
        assert_eq!(score(&[], &c).unwrap().score, 0);
    }

    #[test]
    fn matching_is_case_insensitive_for_non_ascii() {
        let c = cand(&[("Ärende Översikt", weight::TAG)], &[]);
        assert!(score(&parse_query("översikt"), &c).is_some());
    }
}
