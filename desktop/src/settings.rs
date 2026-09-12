//! What squint has been told to do, kept in `settings.json`.
//!
//! The file is the real home of the settings; Tools ▸ Settings is an editor
//! for it (see [`settings_form`](crate::settings_form)), and the file can be
//! edited by hand — in squint, which the dialog offers to do — just as well.
//!
//! Reading is forgiving in every direction: a section the file does not
//! mention is its defaults, a value out of range is brought back into it by
//! [`Settings::sane`], and the two settings squint kept before there were
//! sections are still read from where they were written.

use denise::{Color, ColorScheme, Theme, theme};
use serde::{Deserialize, Serialize};
use squint_core::format::{Indent, Newline, Style};
use std::collections::HashMap;
use std::sync::Mutex;

/// The file they are kept in.
pub const FILE: &str = "settings.json";

/// Everything the dialog edits, a section per tab of it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Settings {
    pub general: General,
    pub editor: Editor,
    pub appearance: Appearance,
    pub highlighting: Highlighting,
    pub formatting: Formatting,
}

/// Opening, reopening and keeping an eye on files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Whether the tabs open when squint closes are open again when it starts.
    pub reopen_tabs: bool,
    /// Whether a reopened tab goes back to the line it was on.
    pub reopen_at_line: bool,
    /// What opening a file does to the row of tabs.
    pub open_in: OpenIn,
    /// How many files File ▸ Open Recent lists. None kept turns it off.
    pub recent_files: usize,
    /// Whether the tabs' files are watched for changes made by something else.
    pub watch_files: bool,
    /// How often they are looked at.
    pub watch_seconds: u32,
    /// Whether a file something else changed asks before it is reloaded. Off,
    /// a tab with no unsaved changes reloads quietly; one with changes still
    /// asks, so nothing typed is lost.
    pub ask_before_reloading: bool,
}

/// Where a file opened goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpenIn {
    /// Into the empty untitled tab in front, if that is what is there, and a
    /// new tab otherwise.
    #[default]
    ReuseEmptyTab,
    /// Always a new tab.
    NewTab,
}

impl OpenIn {
    pub const ALL: [OpenIn; 2] = [OpenIn::ReuseEmptyTab, OpenIn::NewTab];

    pub fn label(self) -> &'static str {
        match self {
            OpenIn::ReuseEmptyTab => "the empty tab, when there is one",
            OpenIn::NewTab => "always a new tab",
        }
    }
}

/// The text itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Editor {
    pub line_numbers: bool,
    /// Tab stops, where no `.editorconfig` says otherwise.
    pub tab_width: u8,
    /// Whether tab stops come from the file's project's `.editorconfig`.
    pub follow_editorconfig: bool,
    /// Whether a file opens read only.
    pub read_only: bool,
}

/// The theme and the faces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// A built-in theme's name — `dark`, `light`, `high-contrast` — or one of
    /// [`custom_themes`](Appearance::custom_themes).
    pub theme: String,
    /// Themes written here rather than built in, which the dialog makes and
    /// edits.
    pub custom_themes: Vec<CustomTheme>,
    /// The monospace face the text is drawn in, by the name
    /// [`fonts`](crate::fonts) lists, or none for the first one found.
    pub text_font: Option<String>,
    /// Its size in logical pixels, which Zoom In and Zoom Out step from.
    pub text_size: u16,
    /// The face the menus, tabs and status line are drawn in.
    pub ui_font: Option<String>,
    pub ui_size: u16,
}

/// A theme written in the settings file: a name, whether it is light or dark,
/// and the nine seed colours DeniseUI derives the rest from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomTheme {
    pub name: String,
    /// Whether the derived surfaces step darker or lighter.
    pub dark: bool,
    pub base: String,
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub neutral: String,
    pub info: String,
    pub success: String,
    pub warning: String,
    pub error: String,
}

/// Syntax highlighting: syntect's grammars and themes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Highlighting {
    pub enabled: bool,
    /// One of syntect's themes, by name.
    pub theme: String,
    /// A file bigger than this many megabytes is not coloured at all: the
    /// grammars would be loaded for a file nobody reads as source.
    pub max_mb: u64,
}

/// How ⇧⌘F lays out what it writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Formatting {
    /// Whether the layout comes from the source file's project's
    /// `.editorconfig`, where there is one. The rest of this section is what
    /// is used when it does not, or when there is none.
    pub follow_editorconfig: bool,
    pub indent: IndentStyle,
    /// How many spaces one level is, ignored for tabs.
    pub indent_size: u8,
    pub newline: NewlineStyle,
    /// Whether the output ends with a line break.
    pub final_newline: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndentStyle {
    #[default]
    Spaces,
    Tabs,
}

impl IndentStyle {
    pub const ALL: [IndentStyle; 2] = [IndentStyle::Spaces, IndentStyle::Tabs];

    pub fn label(self) -> &'static str {
        match self {
            IndentStyle::Spaces => "spaces",
            IndentStyle::Tabs => "tabs",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NewlineStyle {
    #[default]
    Lf,
    CrLf,
    Cr,
}

impl NewlineStyle {
    pub const ALL: [NewlineStyle; 3] = [NewlineStyle::Lf, NewlineStyle::CrLf, NewlineStyle::Cr];

    pub fn label(self) -> &'static str {
        match self {
            NewlineStyle::Lf => "LF",
            NewlineStyle::CrLf => "CRLF",
            NewlineStyle::Cr => "CR",
        }
    }

    fn newline(self) -> Newline {
        match self {
            NewlineStyle::Lf => Newline::Lf,
            NewlineStyle::CrLf => Newline::CrLf,
            NewlineStyle::Cr => Newline::Cr,
        }
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            reopen_tabs: true,
            reopen_at_line: true,
            open_in: OpenIn::default(),
            recent_files: 10,
            watch_files: true,
            watch_seconds: 2,
            ask_before_reloading: true,
        }
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            line_numbers: true,
            tab_width: 4,
            follow_editorconfig: true,
            read_only: false,
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            custom_themes: Vec::new(),
            text_font: None,
            text_size: 13,
            ui_font: None,
            ui_size: 13,
        }
    }
}

impl Default for CustomTheme {
    /// A copy of the built-in dark theme, which is what New Theme starts from.
    fn default() -> Self {
        Self {
            name: "custom".into(),
            dark: true,
            base: "#1E1E2E".into(),
            primary: "#89B4FA".into(),
            secondary: "#F5C2E7".into(),
            accent: "#94E2D5".into(),
            neutral: "#585B70".into(),
            info: "#89DCEB".into(),
            success: "#A6E3A1".into(),
            warning: "#F9E2AF".into(),
            error: "#F38BA8".into(),
        }
    }
}

impl Default for Highlighting {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: squint_core::syntax::DEFAULT_THEME.into(),
            max_mb: 64,
        }
    }
}

impl Default for Formatting {
    fn default() -> Self {
        Self {
            follow_editorconfig: true,
            indent: IndentStyle::Spaces,
            indent_size: 2,
            newline: NewlineStyle::Lf,
            final_newline: true,
        }
    }
}

/// The names of the themes that are always there.
pub const BUILT_IN_THEMES: [&str; 3] = ["dark", "light", "high-contrast"];

impl Settings {
    /// The settings with every value inside the range that means anything:
    /// what a file edited by hand is put through before it is used.
    pub fn sane(mut self) -> Self {
        self.general.recent_files = self.general.recent_files.min(50);
        self.general.watch_seconds = self.general.watch_seconds.clamp(1, 60);
        self.editor.tab_width = self.editor.tab_width.clamp(1, 16);
        self.appearance.text_size = self.appearance.text_size.clamp(6, 64);
        self.appearance.ui_size = self.appearance.ui_size.clamp(8, 32);
        self.highlighting.max_mb = self.highlighting.max_mb.clamp(1, 4096);
        self.formatting.indent_size = self.formatting.indent_size.clamp(1, 16);
        self.appearance.custom_themes.retain(|t| !t.name.is_empty());
        self
    }

    /// Every theme that can be chosen: the built-in ones, then the file's own.
    pub fn theme_names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILT_IN_THEMES.iter().map(|n| (*n).to_string()).collect();
        names.extend(self.appearance.custom_themes.iter().map(|t| t.name.clone()));
        names
    }

    /// The theme itself, unscaled: the built-in one of that name, the custom
    /// one of that name, or the dark one.
    pub fn theme(&self) -> Theme {
        match self.appearance.theme.as_str() {
            "light" => theme::LIGHT,
            "high-contrast" => theme::HIGH_CONTRAST,
            "dark" => theme::DARK,
            name => self
                .appearance
                .custom_themes
                .iter()
                .find(|t| t.name == name)
                .map_or(theme::DARK, CustomTheme::theme),
        }
    }

    /// The layout ⇧⌘F uses where no `.editorconfig` decides it.
    pub fn format_style(&self) -> Style {
        Style {
            indent: match self.formatting.indent {
                IndentStyle::Tabs => Indent::Tab,
                IndentStyle::Spaces => Indent::Spaces(self.formatting.indent_size.max(1)),
            },
            newline: self.formatting.newline.newline(),
            final_newline: self.formatting.final_newline,
        }
    }
}

impl CustomTheme {
    /// The theme DeniseUI derives from these nine colours. Its name is leaked,
    /// once per name: a theme's name is `&'static str` in the toolkit, and a
    /// dialog that renames a theme twenty times should leak twenty names, not
    /// one per keystroke.
    pub fn theme(&self) -> Theme {
        let scheme = if self.dark {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        };
        Theme::from_seeds(
            interned(&self.name),
            scheme,
            color_of(&self.base, Color::rgb(0x1E, 0x1E, 0x2E)),
            color_of(&self.primary, Color::rgb(0x89, 0xB4, 0xFA)),
            color_of(&self.secondary, Color::rgb(0xF5, 0xC2, 0xE7)),
            color_of(&self.accent, Color::rgb(0x94, 0xE2, 0xD5)),
            color_of(&self.neutral, Color::rgb(0x58, 0x5B, 0x70)),
            color_of(&self.info, Color::rgb(0x89, 0xDC, 0xEB)),
            color_of(&self.success, Color::rgb(0xA6, 0xE3, 0xA1)),
            color_of(&self.warning, Color::rgb(0xF9, 0xE2, 0xAF)),
            color_of(&self.error, Color::rgb(0xF3, 0x8B, 0xA8)),
        )
    }

    /// The colours in the order the dialog shows them: what each one is
    /// called, and the field it is.
    pub const SEEDS: [&'static str; 9] = [
        "Base",
        "Primary",
        "Secondary",
        "Accent",
        "Neutral",
        "Info",
        "Success",
        "Warning",
        "Error",
    ];

    pub fn seed(&self, n: usize) -> &str {
        match n {
            0 => &self.base,
            1 => &self.primary,
            2 => &self.secondary,
            3 => &self.accent,
            4 => &self.neutral,
            5 => &self.info,
            6 => &self.success,
            7 => &self.warning,
            _ => &self.error,
        }
    }

    pub fn set_seed(&mut self, n: usize, value: String) {
        let field = match n {
            0 => &mut self.base,
            1 => &mut self.primary,
            2 => &mut self.secondary,
            3 => &mut self.accent,
            4 => &mut self.neutral,
            5 => &mut self.info,
            6 => &mut self.success,
            7 => &mut self.warning,
            _ => &mut self.error,
        };
        *field = value;
    }
}

/// `#1E1E2E`, `1e1e2e` or `#fff` as a colour, or `fallback` for anything else:
/// a half-typed colour in a field must not change what is on screen to black.
pub fn color_of(text: &str, fallback: Color) -> Color {
    let hex = text.trim().trim_start_matches('#');
    let value = match hex.len() {
        3 => {
            let mut wide = String::with_capacity(6);
            for c in hex.chars() {
                wide.push(c);
                wide.push(c);
            }
            u32::from_str_radix(&wide, 16)
        }
        6 => u32::from_str_radix(hex, 16),
        _ => return fallback,
    };
    value.map_or(fallback, Color::from_rgb888)
}

/// The names themes have been given, kept so each is leaked once.
static NAMES: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);

fn interned(name: &str) -> &'static str {
    let Ok(mut names) = NAMES.lock() else {
        return "custom";
    };
    let names = names.get_or_insert_with(HashMap::new);
    if let Some(interned) = names.get(name) {
        return interned;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.insert(name.to_string(), leaked);
    leaked
}

/// The file as it is read: the sections, and the two settings squint wrote at
/// the top level before there were sections.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Raw {
    general: General,
    editor: Editor,
    appearance: Appearance,
    highlighting: Highlighting,
    formatting: Formatting,
    reopen_tabs: Option<bool>,
    ask_before_reloading: Option<bool>,
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Raw::deserialize(deserializer)?;
        let mut settings = Settings {
            general: raw.general,
            editor: raw.editor,
            appearance: raw.appearance,
            highlighting: raw.highlighting,
            formatting: raw.formatting,
        };
        // Written by a squint from before the sections: the settings dialog
        // writes them where they belong the first time it saves.
        if let Some(on) = raw.reopen_tabs {
            settings.general.reopen_tabs = on;
        }
        if let Some(on) = raw.ask_before_reloading {
            settings.general.ask_before_reloading = on;
        }
        Ok(settings.sane())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A setting added later is its default in a file written before it was.
    #[test]
    fn a_setting_the_file_does_not_mention_is_its_default() {
        let settings: Settings =
            serde_json::from_str(r#"{"general": {"reopen_tabs": false}}"#).expect("json");
        assert!(!settings.general.reopen_tabs);
        assert!(settings.general.ask_before_reloading);
        assert_eq!(settings.editor, Editor::default());
        assert_eq!(settings.appearance.theme, "dark");
    }

    /// The two settings squint kept before there were sections are still read.
    #[test]
    fn a_file_from_before_the_sections_is_read_where_it_was_written() {
        let settings: Settings =
            serde_json::from_str(r#"{"reopen_tabs": false, "ask_before_reloading": false}"#)
                .expect("json");
        assert!(!settings.general.reopen_tabs);
        assert!(!settings.general.ask_before_reloading);
    }

    /// What is written comes back, in the sections it was written in.
    #[test]
    fn what_is_written_is_read_again() {
        let mut settings = Settings::default();
        settings.appearance.text_size = 18;
        settings.appearance.custom_themes.push(CustomTheme {
            name: "midnight".into(),
            ..CustomTheme::default()
        });
        settings.formatting.indent = IndentStyle::Tabs;
        let json = serde_json::to_string(&settings).expect("write");
        let again: Settings = serde_json::from_str(&json).expect("read");
        assert_eq!(again, settings);
        assert_eq!(
            again.theme_names(),
            ["dark", "light", "high-contrast", "midnight"]
        );
    }

    /// A value a hand-edited file puts out of range is brought back into it.
    #[test]
    fn a_value_out_of_range_is_brought_back_into_it() {
        let settings: Settings = serde_json::from_str(
            r#"{"editor": {"tab_width": 200}, "appearance": {"text_size": 900}}"#,
        )
        .expect("json");
        assert_eq!(settings.editor.tab_width, 16);
        assert_eq!(settings.appearance.text_size, 64);
    }

    /// A custom theme is the theme it names, and a name nobody knows is dark.
    #[test]
    fn a_theme_is_the_one_its_name_picks() {
        let mut settings = Settings::default();
        assert_eq!(settings.theme().name, "dark");
        settings.appearance.theme = "light".into();
        assert_eq!(settings.theme().name, "light");
        settings.appearance.theme = "nothing like it".into();
        assert_eq!(settings.theme().name, "dark");
        settings.appearance.custom_themes.push(CustomTheme {
            name: "midnight".into(),
            base: "#000000".into(),
            ..CustomTheme::default()
        });
        settings.appearance.theme = "midnight".into();
        let theme = settings.theme();
        assert_eq!(theme.name, "midnight");
        assert_eq!(theme.color(denise::Role::Base100), Color::rgb(0, 0, 0));
    }

    #[test]
    fn a_colour_is_read_in_every_spelling_and_never_read_wrong() {
        assert_eq!(color_of("#1E1E2E", Color::WHITE), Color::rgb(30, 30, 46));
        assert_eq!(color_of("1e1e2e", Color::WHITE), Color::rgb(30, 30, 46));
        assert_eq!(color_of("#fff", Color::WHITE), Color::rgb(255, 255, 255));
        assert_eq!(color_of("#12", Color::WHITE), Color::WHITE, "half typed");
        assert_eq!(color_of("nonsense", Color::WHITE), Color::WHITE);
    }

    /// The formatting section is the style ⇧⌘F uses without an .editorconfig.
    #[test]
    fn the_formatting_section_is_a_style() {
        let mut settings = Settings::default();
        assert_eq!(settings.format_style(), Style::default());
        settings.formatting.indent = IndentStyle::Tabs;
        settings.formatting.newline = NewlineStyle::CrLf;
        settings.formatting.final_newline = false;
        let style = settings.format_style();
        assert_eq!(style.indent, Indent::Tab);
        assert_eq!(style.newline, Newline::CrLf);
        assert!(!style.final_newline);
    }
}
