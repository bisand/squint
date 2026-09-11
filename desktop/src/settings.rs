//! The choices the Settings menu ticks.

use serde::{Deserialize, Serialize};

/// The file they are kept in.
pub const FILE: &str = "settings.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Whether the tabs open when squint closed are open again when it starts.
    pub reopen_tabs: bool,
    /// Whether a file something else changed asks before it is reloaded. Off,
    /// a tab with no unsaved changes reloads quietly; one with changes still
    /// asks, so nothing typed is lost.
    pub ask_before_reloading: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            reopen_tabs: true,
            ask_before_reloading: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A setting added later is its default in a file written before it was.
    #[test]
    fn a_setting_the_file_does_not_mention_is_its_default() {
        let settings: Settings = serde_json::from_str(r#"{"reopen_tabs": false}"#).expect("json");
        assert!(!settings.reopen_tabs);
        assert!(settings.ask_before_reloading);
    }
}
