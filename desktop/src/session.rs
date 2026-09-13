//! The tabs that were open and the window they were in, so the next run can
//! open them again the way they were left, and the colours a tab can be given.

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
    /// The window they were in, as it was left. `None` before the first run
    /// that wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
}

/// The window as a run left it, so the next one opens the same way.
///
/// Two kinds of pixel, because the two facts are not the same kind of fact.
/// The size is logical: it says how much of the desk the window covers, which
/// is what should stay the same when the file it holds is opened on a Retina
/// display instead of an external one. The corner is physical, because a desk
/// spanning displays of different DPI has no single logical grid to name a
/// point in — and physical is what the window system reports and takes back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Window {
    /// How wide the window was inside its frame, in logical pixels.
    pub width: u32,
    /// How tall it was inside its frame, in logical pixels.
    pub height: u32,
    /// Its top-left corner, frame included, in the desktop's physical pixels,
    /// or `None` on a system that will not say where its windows are — which
    /// is Wayland, by design.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Spot>,
    /// Whether it was left maximised. The size and the corner are then the
    /// ones it goes back to when it is un-maximised, not the screen's.
    pub maximized: bool,
}

/// A corner of the desktop, in physical pixels. Negative on a display left of
/// or above the primary one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spot {
    pub x: i32,
    pub y: i32,
}

impl Window {
    /// The smallest window worth opening again: anything smaller is a file
    /// that has been edited or truncated into nonsense, and the defaults are
    /// a better answer than a window nobody can use.
    const LEAST: (u32, u32) = (400, 300);

    /// What this says, when what it says is a window that can be opened.
    pub fn sane(self) -> Option<Self> {
        (self.width >= Self::LEAST.0 && self.height >= Self::LEAST.1).then_some(self)
    }
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
            window: Some(Window {
                width: 1280,
                height: 800,
                at: Some(Spot { x: -1920, y: 40 }),
                maximized: false,
            }),
        };
        let json = serde_json::to_string(&session).expect("json");
        assert!(json.contains(r#""color":"teal""#), "{json}");
        assert!(json.contains(r#""at":{"x":-1920,"y":40}"#), "{json}");
        assert!(!json.contains("null"), "nothing written that says nothing");
        assert_eq!(
            serde_json::from_str::<Session>(&json).expect("back"),
            session
        );
    }

    #[test]
    fn a_session_from_before_the_window_was_kept_still_reads() {
        let session: Session =
            serde_json::from_str(r#"{"tabs":[],"active":0}"#).expect("the old shape");
        assert_eq!(session.window, None, "and says nothing about a window");
    }

    #[test]
    fn a_window_too_small_to_use_is_not_worth_opening_again() {
        let left = Window {
            width: 1000,
            height: 700,
            at: None,
            maximized: false,
        };
        assert_eq!(left.sane(), Some(left));
        assert_eq!(Window::default().sane(), None, "nothing was written yet");
        assert_eq!(
            Window {
                width: 40,
                height: 20,
                ..left
            }
            .sane(),
            None,
            "a file somebody has been editing by hand"
        );
    }
}
