//! The tabs that were open, so the next run can open them again, and the
//! colours a tab can be given.

use denise::Color;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The file the tabs are kept in.
pub const FILE: &str = "session.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// The tabs with files, in the order they were along the row.
    pub tabs: Vec<SavedTab>,
    /// Which of them was in front.
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTab {
    pub path: PathBuf,
    /// The name it was given, when it was given one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<TabColor>,
    /// The line the caret was on, counting from 0.
    #[serde(default)]
    pub line: usize,
}

/// A colour a tab can be given. A short list rather than any colour at all:
/// colours are for telling tabs apart at a glance, and nine that are plainly
/// different do that better than a picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Gray,
}

impl TabColor {
    pub const ALL: [TabColor; 9] = [
        TabColor::Red,
        TabColor::Orange,
        TabColor::Yellow,
        TabColor::Green,
        TabColor::Teal,
        TabColor::Blue,
        TabColor::Purple,
        TabColor::Pink,
        TabColor::Gray,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TabColor::Red => "Red",
            TabColor::Orange => "Orange",
            TabColor::Yellow => "Yellow",
            TabColor::Green => "Green",
            TabColor::Teal => "Teal",
            TabColor::Blue => "Blue",
            TabColor::Purple => "Purple",
            TabColor::Pink => "Pink",
            TabColor::Gray => "Gray",
        }
    }

    pub fn color(self) -> Color {
        match self {
            TabColor::Red => Color::rgb(229, 72, 77),
            TabColor::Orange => Color::rgb(247, 144, 9),
            TabColor::Yellow => Color::rgb(245, 208, 0),
            TabColor::Green => Color::rgb(48, 164, 108),
            TabColor::Teal => Color::rgb(18, 165, 148),
            TabColor::Blue => Color::rgb(62, 99, 221),
            TabColor::Purple => Color::rgb(142, 78, 198),
            TabColor::Pink => Color::rgb(214, 64, 159),
            TabColor::Gray => Color::rgb(128, 128, 128),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_written_the_way_it_is_read() {
        let session = Session {
            tabs: vec![
                SavedTab {
                    path: "/var/log/system.log".into(),
                    name: Some("system".into()),
                    color: Some(TabColor::Teal),
                    line: 1200,
                },
                SavedTab {
                    path: "/tmp/dump.json".into(),
                    name: None,
                    color: None,
                    line: 0,
                },
            ],
            active: 1,
        };
        let json = serde_json::to_string(&session).expect("json");
        assert!(json.contains(r#""color":"teal""#), "{json}");
        assert!(!json.contains("null"), "nothing written that says nothing");
        assert_eq!(
            serde_json::from_str::<Session>(&json).expect("back"),
            session
        );
    }
}
