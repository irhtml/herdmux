use std::collections::HashMap;

/// `/rename` names by session id, filled by the background scan and put
/// on the panes after every snapshot.
#[derive(Debug, Clone, Default)]
pub struct SessionNamesState {
    pub names: HashMap<String, String>,
}
