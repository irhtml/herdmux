/// How much each agent row shows. Shared by every sidebar through
/// `@sidebar_view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Every row shows all its details.
    #[default]
    Full,
    /// One line per agent; only the selected (or focused) agent shows
    /// its details.
    Compact,
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
        }
    }

    /// Unknown values fall back to `Full`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "compact" => Self::Compact,
            _ => Self::Full,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Full => Self::Compact,
            Self::Compact => Self::Full,
        }
    }
}

/// Order of the agent list. Shared by every sidebar through `@sidebar_sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// Grouped under repository headers, repositories in name order.
    #[default]
    Repo,
    /// One list, most recently active agent first.
    Recent,
}

impl SortMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Recent => "recent",
        }
    }

    /// Unknown values fall back to `Repo`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "recent" => Self::Recent,
            _ => Self::Repo,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Repo => Self::Recent,
            Self::Recent => Self::Repo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip_and_default() {
        for mode in [ViewMode::Full, ViewMode::Compact] {
            assert_eq!(ViewMode::from_label(mode.as_str()), mode);
        }
        for mode in [SortMode::Repo, SortMode::Recent] {
            assert_eq!(SortMode::from_label(mode.as_str()), mode);
        }
        assert_eq!(ViewMode::from_label(""), ViewMode::Full);
        assert_eq!(SortMode::from_label("bogus"), SortMode::Repo);
        assert_eq!(ViewMode::Full.toggled(), ViewMode::Compact);
        assert_eq!(SortMode::Recent.toggled(), SortMode::Repo);
    }
}
