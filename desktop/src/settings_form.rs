//! What the settings window holds: a section down the left, the rows of that
//! section to the right, and Save, Apply and Cancel along the bottom.
//!
//! The form fills a window of its own — see
//! [`settings_window`](crate::settings_window) — rather than covering the text,
//! which is where every desktop keeps its settings. It knows nothing about the
//! window it is in beyond the tree it builds into.
//!
//! It edits a copy — the draft — and never the settings the editor is running
//! on. Save and Apply hand the draft back through [`Outcome`], and the window
//! passes it to the editor, which writes it to `settings.json` and applies it;
//! Cancel drops it. The file is the settings' real home, and Edit the File…
//! opens it in a tab, so this is an editor for a file that can be edited any
//! other way too.
//!
//! Controls are read from the tree rather than reported by messages: a
//! checkbox's message is a fn pointer and cannot say which checkbox it is, so
//! every control this built is kept in [`Form::controls`] and read back
//! whenever anything happens. What is on screen is the draft, always.

use crate::fonts;
use crate::settings::{
    Appearance, BUILT_IN_THEMES, CustomTheme, Editor, Formatting, General, Highlighting,
    IndentStyle, NewlineStyle, OpenIn, Settings,
};
use denise::{Color, Point, Rect, Role, Size};
use denise_text::TextStyle;
use denise_ui::widgets::{Button, Checkbox, Label, List, Panel, Select, TextInput};
use denise_ui::{NodeId, Side, Ui};
use std::path::PathBuf;

/// What the dialog says to the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormMsg {
    /// A section was picked from the list down the left.
    Section(usize),
    /// A checkbox was ticked: which one is read from the tree.
    Ticked(bool),
    /// A dropdown asked to be opened; the control it is.
    OpenSelect(usize),
    /// A row of the open dropdown was chosen.
    Chose(usize),
    /// Enter in a field: takes what is typed and shows it.
    Submit,
    Button(Action),
}

/// A button of the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Save,
    Apply,
    Cancel,
    /// Every setting back to what squint ships with.
    Restore,
    /// Opens `settings.json` in a tab.
    EditFile,
    NewTheme,
    DuplicateTheme,
    DeleteTheme,
    /// Picks a font file for the text, or for the chrome.
    ChooseTextFont,
    ChooseUiFont,
}

/// What the window should do about what just happened in the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Write the draft and apply it, and close.
    Save,
    /// Write the draft and apply it, and stay open.
    Apply,
    Close,
    /// Write the draft, then open the settings file in a tab.
    EditFile,
    /// Pick a font file for the text, or for the chrome.
    PickFont {
        text: bool,
    },
}

/// The sections, in the order the list down the left shows them.
pub const SECTIONS: [&str; 5] = [
    "General",
    "Editor",
    "Appearance",
    "Highlighting",
    "Formatting",
];

/// Which setting a control is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    ReopenTabs,
    ReopenAtLine,
    OpenIn,
    RecentFiles,
    WatchFiles,
    WatchSeconds,
    AskBeforeReloading,
    LineNumbers,
    TabWidth,
    EditorConfigTabs,
    ReadOnly,
    ThemeChoice,
    TextFont,
    TextSize,
    UiFont,
    UiSize,
    ThemeName,
    ThemeDark,
    /// One of the nine seed colours of the custom theme being edited.
    ThemeSeed(usize),
    HighlightOn,
    SyntaxTheme,
    MaxMb,
    FormatEditorConfig,
    Indent,
    IndentSize,
    Newline,
    FinalNewline,
}

/// What kind of widget a control is, which is how it is read back.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Check,
    /// The options, so a chosen row is the value it stands for.
    Select(Vec<String>),
    Text,
}

struct Control {
    field: Field,
    node: NodeId,
    kind: Kind,
}

pub struct Form {
    /// The settings as the form has them, which the editor only sees on Save
    /// or Apply.
    draft: Settings,
    section: usize,
    /// Everything the form has built, under the window's root, so a resize
    /// can drop the lot and lay it out again.
    frame: NodeId,
    /// The scrolling page the rows are in.
    page: NodeId,
    sections: NodeId,
    controls: Vec<Control>,
    /// The control whose dropdown is open, while one is.
    open: Option<usize>,
    /// Where the next row goes, while a page is being built.
    y: i32,
    page_w: i32,
    scale: f32,
    /// The size the dialog was laid out for; a different one is rebuilt.
    size: Size,
    file: Option<PathBuf>,
    /// The face the chrome is drawn in, which the form is drawn in too.
    chrome: TextStyle,
    /// The themes syntect has, read the first time they are shown: loading
    /// the grammars to list them would hold up the dialog opening.
    syntax_themes: Vec<String>,
}

/// The message a checkbox sends. A fn pointer cannot say which checkbox it
/// is; the tree is read instead.
fn ticked(on: bool) -> FormMsg {
    FormMsg::Ticked(on)
}

fn section_picked(index: usize) -> FormMsg {
    FormMsg::Section(index)
}

fn chose(row: usize) -> FormMsg {
    FormMsg::Chose(row)
}

impl Form {
    /// Builds the form into `ui`, filling it, editing a copy of `settings`.
    pub fn build_in(
        ui: &mut Ui<FormMsg>,
        settings: &Settings,
        file: Option<PathBuf>,
        scale: f32,
        chrome: TextStyle,
    ) -> Option<Self> {
        let root = ui.root();
        let mut form = Self {
            draft: settings.clone(),
            section: 0,
            frame: root,
            page: root,
            sections: root,
            controls: Vec::new(),
            open: None,
            y: 0,
            page_w: 0,
            scale,
            size: ui.size(),
            file,
            chrome,
            syntax_themes: Vec::new(),
        };
        form.build(ui)?;
        Some(form)
    }

    pub fn draft(&self) -> &Settings {
        &self.draft
    }

    /// The draft, to be changed as a control would change it.
    #[cfg(test)]
    pub fn draft_mut(&mut self) -> &mut Settings {
        &mut self.draft
    }

    /// Shows the section of that name, for a snapshot. Whether there is one.
    pub fn show_section(&mut self, name: &str, ui: &mut Ui<FormMsg>) -> bool {
        let Some(index) = SECTIONS.iter().position(|s| s.eq_ignore_ascii_case(name)) else {
            return false;
        };
        self.handle(FormMsg::Section(index), ui);
        true
    }

    /// Sets the font a file dialog picked, and shows it.
    pub fn set_font(&mut self, ui: &mut Ui<FormMsg>, text: bool, name: String) {
        if text {
            self.draft.appearance.text_font = Some(name);
        } else {
            self.draft.appearance.ui_font = Some(name);
        }
        self.rebuild(ui);
    }

    /// Lays the form out again when its window has been resized.
    pub fn resized(&mut self, ui: &mut Ui<FormMsg>) {
        if ui.size() == self.size {
            return;
        }
        self.read(ui);
        self.size = ui.size();
        let frame = self.frame;
        ui.remove(frame);
        let _ = self.build(ui);
    }

    // ---- what happens ------------------------------------------------------

    /// Acts on something the dialog said. What the window should do about it,
    /// if anything.
    pub fn handle(&mut self, msg: FormMsg, ui: &mut Ui<FormMsg>) -> Option<Outcome> {
        match msg {
            FormMsg::Section(index) => {
                self.read(ui);
                if index < SECTIONS.len() && index != self.section {
                    self.section = index;
                    self.rebuild(ui);
                }
                None
            }
            FormMsg::Ticked(_) => {
                let was = self.shape();
                self.read(ui);
                self.preview(ui);
                // A tick can decide what else the page shows — whether a
                // custom theme is being edited, whether there is anything to
                // colour — and the page is built again only when it does.
                if self.shape() != was {
                    self.rebuild(ui);
                }
                None
            }
            // Enter in a field: what was typed may be a theme's name or one of
            // its colours, so the page is built again around it.
            FormMsg::Submit => {
                self.read(ui);
                self.preview(ui);
                self.rebuild(ui);
                None
            }
            FormMsg::OpenSelect(index) => {
                self.read(ui);
                if let Some(control) = self.controls.get(index) {
                    let node = control.node;
                    if open_dropdown(ui, node, chose).is_some() {
                        self.open = Some(index);
                    }
                }
                None
            }
            FormMsg::Chose(row) => {
                ui.close_popup();
                let index = self.open.take()?;
                if let Some(control) = self.controls.get(index)
                    && let Some(select) = ui.widget_mut::<Select<FormMsg>>(control.node)
                {
                    select.set_selected(Some(row));
                }
                let was = self.shape();
                self.read(ui);
                self.preview(ui);
                if self.shape() != was {
                    self.rebuild(ui);
                }
                None
            }
            FormMsg::Button(action) => self.button(action, ui),
        }
    }

    fn button(&mut self, action: Action, ui: &mut Ui<FormMsg>) -> Option<Outcome> {
        self.read(ui);
        match action {
            Action::Save => Some(Outcome::Save),
            Action::Apply => Some(Outcome::Apply),
            Action::Cancel => Some(Outcome::Close),
            Action::EditFile => Some(Outcome::EditFile),
            Action::ChooseTextFont => Some(Outcome::PickFont { text: true }),
            Action::ChooseUiFont => Some(Outcome::PickFont { text: false }),
            Action::Restore => {
                let themes = std::mem::take(&mut self.draft.appearance.custom_themes);
                self.draft = Settings::default();
                // The themes somebody wrote are theirs, not a setting: putting
                // the settings back must not throw their work away.
                self.draft.appearance.custom_themes = themes;
                self.preview(ui);
                self.rebuild(ui);
                None
            }
            Action::NewTheme => {
                let theme = CustomTheme {
                    name: self.fresh_theme_name("custom"),
                    ..CustomTheme::default()
                };
                self.draft.appearance.theme = theme.name.clone();
                self.draft.appearance.custom_themes.push(theme);
                self.preview(ui);
                self.rebuild(ui);
                None
            }
            Action::DuplicateTheme => {
                let from = self.draft.theme();
                let name = self.fresh_theme_name(&format!("{} copy", from.name));
                let seed = |role: Role| hex_of(from.color(role));
                let theme = CustomTheme {
                    name: name.clone(),
                    dark: from.scheme == denise::ColorScheme::Dark,
                    base: seed(Role::Base100),
                    primary: seed(Role::Primary),
                    secondary: seed(Role::Secondary),
                    accent: seed(Role::Accent),
                    neutral: seed(Role::Neutral),
                    info: seed(Role::Info),
                    success: seed(Role::Success),
                    warning: seed(Role::Warning),
                    error: seed(Role::Error),
                };
                self.draft.appearance.theme = name;
                self.draft.appearance.custom_themes.push(theme);
                self.preview(ui);
                self.rebuild(ui);
                None
            }
            Action::DeleteTheme => {
                let name = self.draft.appearance.theme.clone();
                self.draft
                    .appearance
                    .custom_themes
                    .retain(|t| t.name != name);
                if !self.draft.theme_names().contains(&name) {
                    self.draft.appearance.theme = "dark".into();
                }
                self.preview(ui);
                self.rebuild(ui);
                None
            }
        }
    }

    /// A name no theme has yet: `custom`, then `custom 2`.
    fn fresh_theme_name(&self, from: &str) -> String {
        let taken = self.draft.theme_names();
        if !taken.iter().any(|n| n == from) {
            return from.to_string();
        }
        (2..)
            .map(|n| format!("{from} {n}"))
            .find(|name| !taken.iter().any(|n| n == name))
            .unwrap_or_else(|| from.to_string())
    }

    /// The theme as the draft has it, so an edit is seen while it is made.
    fn preview(&mut self, ui: &mut Ui<FormMsg>) {
        let theme = self.draft.theme().scaled(self.scale);
        if *ui.theme() != theme {
            ui.set_theme(theme);
        }
    }

    /// What decides which rows the page has: the section, whether a theme of
    /// the file's own is being edited, and whether there is any highlighting
    /// to set. A change to one of these is a page built again.
    fn shape(&self) -> (usize, Option<usize>, bool) {
        (
            self.section,
            self.editing_theme(),
            self.draft.highlighting.enabled,
        )
    }

    /// The custom theme being edited, if the chosen one is custom.
    fn editing_theme(&self) -> Option<usize> {
        let name = &self.draft.appearance.theme;
        self.draft
            .appearance
            .custom_themes
            .iter()
            .position(|t| &t.name == name)
    }

    // ---- reading the controls ----------------------------------------------

    /// Takes what every control on the page says into the draft.
    fn read(&mut self, ui: &Ui<FormMsg>) {
        let mut draft = std::mem::take(&mut self.draft);
        for control in &self.controls {
            match &control.kind {
                Kind::Check => {
                    let Some(on) = ui
                        .widget::<Checkbox<FormMsg>>(control.node)
                        .map(Checkbox::checked)
                    else {
                        continue;
                    };
                    set_check(&mut draft, control.field, on);
                }
                Kind::Select(options) => {
                    let Some(chosen) = ui
                        .widget::<Select<FormMsg>>(control.node)
                        .and_then(Select::selected)
                        .and_then(|i| options.get(i))
                    else {
                        continue;
                    };
                    set_choice(&mut draft, control.field, chosen);
                }
                Kind::Text => {
                    let Some(text) = ui
                        .widget::<TextInput<FormMsg>>(control.node)
                        .map(|field| field.text().to_string())
                    else {
                        continue;
                    };
                    set_text(&mut draft, control.field, text);
                }
            }
        }
        self.draft = draft.sane();
    }

    // ---- building it -------------------------------------------------------

    /// The card, the list of sections, the buttons and the first page.
    fn build(&mut self, ui: &mut Ui<FormMsg>) -> Option<()> {
        let s = |v: i32| (v as f32 * self.scale + 0.5) as i32;
        let px = |v: f32| (v * self.scale + 0.5) as u16;
        let size = ui.size();
        self.size = size;
        let (w, h) = (size.width as i32, size.height as i32);
        // Everything hangs off one node, so a resize drops the lot and this
        // runs again. The window's own ground is what shows behind it.
        let root = ui.root();
        let frame = ui.add(root, Panel::bare(), Rect::new(0, 0, w, h))?;
        self.frame = frame;

        let pad = s(14);
        // No heading: the window's title bar already says what this is. What
        // it does not say is where the settings are kept.
        let where_it_lives = match &self.file {
            Some(file) => format!("kept in {}", file.display()),
            None => "not written anywhere: this squint keeps nothing".to_string(),
        };
        ui.add(
            frame,
            Label::new(where_it_lives)
                .with_size(px(10.0))
                .with_role(Role::Neutral),
            Rect::new(pad, pad, w - pad * 2, s(14)),
        )?;

        let top = pad + s(22);
        let buttons_h = s(44);
        let body_h = h - top - buttons_h - pad;
        let list_w = s(150);
        let list = List::new(SECTIONS, section_picked)
            .with_selected(Some(self.section))
            .with_row_height(s(28))
            .with_style(TextStyle {
                size_px: px(12.0),
                ..self.chrome
            })
            .with_role(Role::Primary);
        self.sections = ui.add(frame, list, Rect::new(pad, top, list_w, body_h))?;

        let page_x = pad + list_w + s(14);
        self.page_w = w - page_x - pad;
        let page = ui.add(
            frame,
            Panel::bare(),
            Rect::new(page_x, top, self.page_w, body_h),
        )?;
        ui.set_scrollable(page, true);
        self.page = page;

        // The buttons: what closes the window on the right, where a dialog's
        // buttons are, and what does not on the left.
        let by = h - buttons_h + s(4);
        let bw = s(88);
        let bh = s(28);
        let small = TextStyle {
            size_px: px(12.0),
            ..self.chrome
        };
        let mut left = pad;
        for (label, action, width) in [
            ("Restore Defaults", Action::Restore, s(128)),
            ("Edit the File…", Action::EditFile, s(110)),
        ] {
            let button = Button::new(label, FormMsg::Button(action))
                .with_style(small)
                .with_role(Role::Neutral);
            ui.add(frame, button, Rect::new(left, by, width, bh))?;
            left += width + s(8);
        }
        let mut right = w - pad - bw;
        for (label, action, role) in [
            ("Save", Action::Save, Role::Primary),
            ("Apply", Action::Apply, Role::Neutral),
            ("Cancel", Action::Cancel, Role::Neutral),
        ] {
            let button = Button::new(label, FormMsg::Button(action))
                .with_style(small)
                .with_role(role);
            ui.add(frame, button, Rect::new(right, by, bw, bh))?;
            right -= bw + s(8);
        }

        self.fill_page(ui);
        ui.focus(Some(self.sections));
        Some(())
    }

    /// Builds the page again: the rows of the section, as the draft is now.
    /// Whatever had the keyboard has it again, if its row is still there.
    fn rebuild(&mut self, ui: &mut Ui<FormMsg>) {
        let Some(bounds) = ui.layout(self.page) else {
            return;
        };
        let had_focus = self
            .controls
            .iter()
            .find(|c| Some(c.node) == ui.focused())
            .map(|c| c.field);
        ui.remove(self.page);
        self.controls.clear();
        let Some(page) = ui.add(self.frame, Panel::bare(), bounds) else {
            return;
        };
        ui.set_scrollable(page, true);
        self.page = page;
        if let Some(list) = ui.widget_mut::<List<FormMsg>>(self.sections) {
            list.set_selected(Some(self.section));
        }
        self.fill_page(ui);
        if let Some(field) = had_focus
            && let Some(control) = self.controls.iter().find(|c| c.field == field)
        {
            let node = control.node;
            ui.focus(Some(node));
        }
    }

    fn fill_page(&mut self, ui: &mut Ui<FormMsg>) {
        self.y = 0;
        match self.section {
            0 => self.general_page(ui),
            1 => self.editor_page(ui),
            2 => self.appearance_page(ui),
            3 => self.highlighting_page(ui),
            _ => self.formatting_page(ui),
        }
    }

    fn general_page(&mut self, ui: &mut Ui<FormMsg>) {
        let General {
            reopen_tabs,
            reopen_at_line,
            open_in,
            recent_files,
            watch_files,
            watch_seconds,
            ask_before_reloading,
        } = self.draft.general.clone();
        self.heading(ui, "Opening");
        self.check(
            ui,
            Field::ReopenTabs,
            "Reopen tabs at launch",
            "The tabs open when squint closes open again when it starts.",
            reopen_tabs,
        );
        self.check(
            ui,
            Field::ReopenAtLine,
            "Reopen at the same line",
            "A tab comes back where it was, not at the top.",
            reopen_at_line,
        );
        let options: Vec<String> = OpenIn::ALL.iter().map(|o| o.label().to_string()).collect();
        let chosen = OpenIn::ALL.iter().position(|o| *o == open_in);
        self.select(
            ui,
            Field::OpenIn,
            "A file opens in",
            "One editor window with a row of tabs: opening a file never opens another.",
            options,
            chosen,
        );
        self.text(
            ui,
            Field::RecentFiles,
            "Recent files kept",
            "How many File ▸ Open Recent lists. None keeps no list at all.",
            recent_files.to_string(),
        );

        self.heading(ui, "Files changed by something else");
        self.check(
            ui,
            Field::WatchFiles,
            "Watch the tabs' files",
            "Each tab's file is looked at now and then, in case something else wrote it.",
            watch_files,
        );
        self.text(
            ui,
            Field::WatchSeconds,
            "Looked at every (seconds)",
            "Between 1 and 60.",
            watch_seconds.to_string(),
        );
        self.check(
            ui,
            Field::AskBeforeReloading,
            "Ask before reloading",
            "Off, a tab with no unsaved changes reloads quietly; one with changes still asks.",
            ask_before_reloading,
        );
    }

    fn editor_page(&mut self, ui: &mut Ui<FormMsg>) {
        let Editor {
            line_numbers,
            tab_width,
            follow_editorconfig,
            read_only,
        } = self.draft.editor.clone();
        self.heading(ui, "The text");
        self.check(
            ui,
            Field::LineNumbers,
            "Line numbers",
            "The gutter down the left. View ▸ Line Numbers is the same setting.",
            line_numbers,
        );
        self.check(
            ui,
            Field::EditorConfigTabs,
            "Tab stops from .editorconfig",
            "The project decides: tab_width, or a numeric indent_size.",
            follow_editorconfig,
        );
        self.text(
            ui,
            Field::TabWidth,
            "Tab stops every",
            "Columns, where no .editorconfig decides it. Between 1 and 16.",
            tab_width.to_string(),
        );
        self.check(
            ui,
            Field::ReadOnly,
            "Open files read only",
            "A tab can be made editable again from Tools ▸ Read Only.",
            read_only,
        );
    }

    fn appearance_page(&mut self, ui: &mut Ui<FormMsg>) {
        let Appearance {
            theme,
            text_font,
            text_size,
            ui_font,
            ui_size,
            ..
        } = self.draft.appearance.clone();
        self.heading(ui, "Theme");
        let names = self.draft.theme_names();
        let chosen = names.iter().position(|n| *n == theme);
        self.select(
            ui,
            Field::ThemeChoice,
            "Theme",
            "Changing it here shows it at once; Cancel puts it back.",
            names,
            chosen,
        );
        // A built-in theme cannot be edited or deleted; duplicating it is how
        // one is started from.
        let custom = !BUILT_IN_THEMES.contains(&theme.as_str());
        self.buttons(
            ui,
            &[
                ("New Theme", Action::NewTheme, true),
                ("Duplicate", Action::DuplicateTheme, true),
                ("Delete", Action::DeleteTheme, custom),
            ],
        );

        if let Some(index) = self.editing_theme() {
            let editing = self.draft.appearance.custom_themes[index].clone();
            self.heading(ui, "This theme");
            self.text(
                ui,
                Field::ThemeName,
                "Name",
                "What the theme is called, here and in the settings file.",
                editing.name.clone(),
            );
            self.check(
                ui,
                Field::ThemeDark,
                "Dark",
                "Whether the surfaces DeniseUI derives step darker or lighter.",
                editing.dark,
            );
            for (n, name) in CustomTheme::SEEDS.iter().enumerate() {
                let hint = if n == 0 {
                    "The nine seed colours, as #RRGGBB. Enter shows what one does."
                } else {
                    ""
                };
                self.text(
                    ui,
                    Field::ThemeSeed(n),
                    name,
                    hint,
                    editing.seed(n).to_string(),
                );
            }
        }

        self.heading(ui, "Faces");
        let mono = self.font_options(fonts::MONO, text_font.as_deref());
        let chosen = font_chosen(&mono, text_font.as_deref());
        self.select(
            ui,
            Field::TextFont,
            "The text",
            "The faces squint looks for that this machine has. Choose a File… takes any TrueType file.",
            mono,
            Some(chosen),
        );
        self.buttons(ui, &[("Choose a File…", Action::ChooseTextFont, true)]);
        self.text(
            ui,
            Field::TextSize,
            "Text size",
            "Logical pixels, and where ⌘0 comes back to. Between 6 and 64.",
            text_size.to_string(),
        );
        let chrome = self.font_options(fonts::UI, ui_font.as_deref());
        let chosen = font_chosen(&chrome, ui_font.as_deref());
        self.select(
            ui,
            Field::UiFont,
            "Menus, tabs and status",
            "The face the chrome is drawn in.",
            chrome,
            Some(chosen),
        );
        self.buttons(ui, &[("Choose a File…", Action::ChooseUiFont, true)]);
        self.text(
            ui,
            Field::UiSize,
            "Chrome size",
            "Logical pixels, between 8 and 32.",
            ui_size.to_string(),
        );
    }

    fn highlighting_page(&mut self, ui: &mut Ui<FormMsg>) {
        let Highlighting {
            enabled,
            theme,
            max_mb,
        } = self.draft.highlighting.clone();
        self.heading(ui, "Syntax highlighting");
        self.check(
            ui,
            Field::HighlightOn,
            "Colour the text",
            "syntect's grammars, loaded on a thread and only for a file that has one.",
            enabled,
        );
        if enabled {
            if self.syntax_themes.is_empty() {
                self.syntax_themes = squint_core::syntax::theme_names();
            }
            let names = self.syntax_themes.clone();
            let chosen = names.iter().position(|n| *n == theme);
            self.select(
                ui,
                Field::SyntaxTheme,
                "Colours",
                "One of the themes syntect ships. It takes effect on Apply.",
                names,
                chosen,
            );
            self.text(
                ui,
                Field::MaxMb,
                "Only files up to (MB)",
                "A bigger file is left uncoloured. Only what is on screen is ever parsed, so raising this costs nothing.",
                max_mb.to_string(),
            );
        }
    }

    fn formatting_page(&mut self, ui: &mut Ui<FormMsg>) {
        let Formatting {
            follow_editorconfig,
            indent,
            indent_size,
            newline,
            final_newline,
        } = self.draft.formatting.clone();
        self.heading(ui, "How ⇧⌘F lays out what it writes");
        self.check(
            ui,
            Field::FormatEditorConfig,
            "Follow .editorconfig",
            "The source file's project decides, where there is one; the rows below are what is used when there is not.",
            follow_editorconfig,
        );
        let options: Vec<String> = IndentStyle::ALL
            .iter()
            .map(|i| i.label().to_string())
            .collect();
        let chosen = IndentStyle::ALL.iter().position(|i| *i == indent);
        self.select(ui, Field::Indent, "Indent with", "", options, chosen);
        self.text(
            ui,
            Field::IndentSize,
            "One level is",
            "Spaces per level, ignored when indenting with tabs. Between 1 and 16.",
            indent_size.to_string(),
        );
        let options: Vec<String> = NewlineStyle::ALL
            .iter()
            .map(|n| n.label().to_string())
            .collect();
        let chosen = NewlineStyle::ALL.iter().position(|n| *n == newline);
        self.select(ui, Field::Newline, "Line breaks", "", options, chosen);
        self.check(
            ui,
            Field::FinalNewline,
            "End with a line break",
            "",
            final_newline,
        );
    }

    /// The faces offered for one kind of text: none, then the ones squint
    /// looks for that this machine has — the ones that suit this text — then
    /// every face installed, and last whatever the settings already name if it
    /// is none of those.
    fn font_options(&self, preferred: &[&str], current: Option<&str>) -> Vec<String> {
        let mut options = vec![AUTOMATIC.to_string()];
        options.extend(fonts::choices(preferred));
        let installed = fonts::installed_names();
        if !installed.is_empty() {
            let already = options.clone();
            options.push(EVERY_FACE.to_string());
            options.extend(
                installed
                    .iter()
                    .filter(|name| !already.contains(name))
                    .cloned(),
            );
        }
        if let Some(current) = current
            && !options.iter().any(|o| o == current)
        {
            options.push(current.to_string());
        }
        options
    }

    // ---- the rows ----------------------------------------------------------

    fn s(&self, v: i32) -> i32 {
        (v as f32 * self.scale + 0.5) as i32
    }

    fn px(&self, v: f32) -> u16 {
        (v * self.scale + 0.5) as u16
    }

    fn heading(&mut self, ui: &mut Ui<FormMsg>, text: &str) {
        if self.y > 0 {
            self.y += self.s(10);
        }
        let h = self.s(18);
        ui.add(
            self.page,
            Label::new(text)
                .with_size(self.px(12.0))
                .with_role(Role::Accent),
            Rect::new(0, self.y, self.page_w, h),
        );
        self.y += h + self.s(4);
    }

    /// A row: its name on the left, a line about it underneath, and the
    /// control on the right. Returns where the control goes.
    fn row(&mut self, ui: &mut Ui<FormMsg>, label: &str, hint: &str) -> Rect {
        let label_w = (self.page_w * 46 / 100).max(self.s(120));
        let control_w = (self.page_w - label_w - self.s(12)).max(self.s(100));
        let line = self.s(20);
        ui.add(
            self.page,
            Label::new(label).with_size(self.px(12.0)),
            Rect::new(0, self.y, label_w, line),
        );
        let mut height = line;
        if !hint.is_empty() {
            // The whole width: a label does not wrap, so a line about a
            // setting has the row to itself, under the control.
            let hint_h = self.s(28);
            ui.add(
                self.page,
                Label::new(hint)
                    .with_size(self.px(10.0))
                    .with_role(Role::Neutral),
                Rect::new(0, self.y + line, self.page_w, hint_h),
            );
            height += hint_h - self.s(6);
        }
        let control = Rect::new(label_w + self.s(12), self.y, control_w, self.s(26));
        self.y += height + self.s(8);
        control
    }

    fn check(&mut self, ui: &mut Ui<FormMsg>, field: Field, label: &str, hint: &str, on: bool) {
        let at = self.row(ui, label, hint);
        let widget = Checkbox::new("", ticked)
            .with_checked(on)
            .with_size(self.px(13.0));
        if let Some(node) = ui.add(self.page, widget, at) {
            self.controls.push(Control {
                field,
                node,
                kind: Kind::Check,
            });
        }
    }

    fn select(
        &mut self,
        ui: &mut Ui<FormMsg>,
        field: Field,
        label: &str,
        hint: &str,
        options: Vec<String>,
        chosen: Option<usize>,
    ) {
        let at = self.row(ui, label, hint);
        let index = self.controls.len();
        let widget = Select::new(options.clone(), FormMsg::OpenSelect(index))
            .with_selected(chosen)
            .with_style(TextStyle {
                size_px: self.px(12.0),
                ..self.chrome
            });
        if let Some(node) = ui.add(self.page, widget, at) {
            self.controls.push(Control {
                field,
                node,
                kind: Kind::Select(options),
            });
        }
    }

    fn text(&mut self, ui: &mut Ui<FormMsg>, field: Field, label: &str, hint: &str, value: String) {
        let at = self.row(ui, label, hint);
        let mut widget = TextInput::new()
            .with_submit(FormMsg::Submit)
            .with_max_chars(120)
            .with_size(self.px(12.0));
        widget.set_text(value);
        if let Some(node) = ui.add(self.page, widget, at) {
            self.controls.push(Control {
                field,
                node,
                kind: Kind::Text,
            });
        }
    }

    /// A row of buttons across the control column, each saying whether it can
    /// be pressed.
    fn buttons(&mut self, ui: &mut Ui<FormMsg>, buttons: &[(&str, Action, bool)]) {
        let at = self.row(ui, "", "");
        let gap = self.s(6);
        let width = ((at.width - gap * (buttons.len() as i32 - 1)) / buttons.len() as i32).max(1);
        for (n, (label, action, enabled)) in buttons.iter().enumerate() {
            let button = Button::new(*label, FormMsg::Button(*action))
                .with_size(self.px(11.0))
                .with_role(Role::Neutral);
            let rect = Rect::new(at.x + n as i32 * (width + gap), at.y, width, self.s(24));
            if let Some(node) = ui.add(self.page, button, rect) {
                ui.set_enabled(node, *enabled);
            }
        }
    }
}

/// What the font dropdown calls no choice at all.
const AUTOMATIC: &str = "automatic";

/// The row between the faces that suit the text and all the rest. It is a
/// heading, not a face: the dropdown will not let it be chosen.
const EVERY_FACE: &str = "— every face installed —";

/// The most rows a dropdown shows before it scrolls, where there is room for
/// that many.
const DROPDOWN_ROWS: i32 = 14;

/// Opens a select's options as a list that scrolls when there are more of them
/// than fit: every face on the machine is a list no screen could drop out
/// whole. DeniseUI's `open_select` is the short version of this — a popup, a
/// panel and a list — with the viewport this one adds so the list can be
/// longer than the popup.
fn open_dropdown(
    ui: &mut Ui<FormMsg>,
    select: NodeId,
    message: fn(usize) -> FormMsg,
) -> Option<NodeId> {
    let widget = ui.widget::<Select<FormMsg>>(select)?;
    let options: Vec<String> = widget.options().to_vec();
    let style = widget.style();
    let chosen = widget.selected();
    if options.is_empty() {
        return None;
    }
    let anchor = ui.bounds(select)?;
    let row = ui.theme().metrics.size_field;
    let widest = options
        .iter()
        .map(|option| ui.text_mut().measure_line(style, option))
        .max()
        .unwrap_or(0);
    // As wide as the control, or as wide as the options need.
    let width = anchor.width.max(widest + row);
    // As tall as the room it has: a popup lives in the window, so one taller
    // than the space above or below its control would hang off the edge and
    // lose its last rows. The side with the most room is the side the popup
    // will flip to, and this is that side's height.
    let surface = ui.size().height as i32;
    let margin = row / 2;
    let room = (surface - anchor.y - anchor.height - margin).max(anchor.y - margin);
    let fits = (room / row.max(1)).clamp(1, DROPDOWN_ROWS);
    let shown = fits.min(options.len() as i32);
    let height = row * shown;
    let full = row * options.len() as i32;

    let container = ui.push_popup(select, Size::new(width as u32, height as u32), Side::Below)?;
    ui.add(container, Panel::default(), Rect::new(0, 0, width, height))?;
    let viewport = ui.add(container, Panel::bare(), Rect::new(0, 0, width, height))?;
    ui.set_scrollable(viewport, true);
    // Inert for selection, wired for activation: the arrows move the highlight
    // and pull the viewport along, and only Enter or a tap reports a choice.
    let mut list = List::inert(options.clone())
        .on_activate(message)
        .with_row_height(row)
        .with_style(style)
        .activate_on_click()
        .with_selected(chosen);
    for (index, option) in options.iter().enumerate() {
        if option == EVERY_FACE {
            list.set_row_enabled(index, false);
        }
    }
    let list = ui.add(viewport, list, Rect::new(0, 0, width, full))?;
    ui.focus(Some(list));
    // A long list opens at what is chosen rather than at its top.
    if let Some(index) = chosen {
        let y = (index as i32 * row - height / 2).clamp(0, (full - height).max(0));
        ui.set_scroll(viewport, Point::new(0, y));
    }
    Some(container)
}

fn font_chosen(options: &[String], current: Option<&str>) -> usize {
    current
        .and_then(|name| options.iter().position(|o| o == name))
        .unwrap_or(0)
}

fn hex_of(color: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b)
}

fn set_check(settings: &mut Settings, field: Field, on: bool) {
    match field {
        Field::ReopenTabs => settings.general.reopen_tabs = on,
        Field::ReopenAtLine => settings.general.reopen_at_line = on,
        Field::WatchFiles => settings.general.watch_files = on,
        Field::AskBeforeReloading => settings.general.ask_before_reloading = on,
        Field::LineNumbers => settings.editor.line_numbers = on,
        Field::EditorConfigTabs => settings.editor.follow_editorconfig = on,
        Field::ReadOnly => settings.editor.read_only = on,
        Field::ThemeDark => {
            if let Some(theme) = editing_mut(settings) {
                theme.dark = on;
            }
        }
        Field::HighlightOn => settings.highlighting.enabled = on,
        Field::FormatEditorConfig => settings.formatting.follow_editorconfig = on,
        Field::FinalNewline => settings.formatting.final_newline = on,
        _ => {}
    }
}

fn set_choice(settings: &mut Settings, field: Field, chosen: &str) {
    match field {
        Field::OpenIn => {
            if let Some(value) = OpenIn::ALL.iter().find(|o| o.label() == chosen) {
                settings.general.open_in = *value;
            }
        }
        Field::ThemeChoice => settings.appearance.theme = chosen.to_string(),
        Field::TextFont | Field::UiFont => {
            if chosen == EVERY_FACE {
                return;
            }
            let face = (chosen != AUTOMATIC).then(|| chosen.to_string());
            if field == Field::TextFont {
                settings.appearance.text_font = face;
            } else {
                settings.appearance.ui_font = face;
            }
        }
        Field::SyntaxTheme => settings.highlighting.theme = chosen.to_string(),
        Field::Indent => {
            if let Some(value) = IndentStyle::ALL.iter().find(|i| i.label() == chosen) {
                settings.formatting.indent = *value;
            }
        }
        Field::Newline => {
            if let Some(value) = NewlineStyle::ALL.iter().find(|n| n.label() == chosen) {
                settings.formatting.newline = *value;
            }
        }
        _ => {}
    }
}

fn set_text(settings: &mut Settings, field: Field, text: String) {
    let number = |fallback: u64| text.trim().parse::<u64>().unwrap_or(fallback);
    match field {
        Field::RecentFiles => {
            settings.general.recent_files = number(settings.general.recent_files as u64) as usize;
        }
        Field::WatchSeconds => {
            settings.general.watch_seconds = number(settings.general.watch_seconds as u64) as u32;
        }
        Field::TabWidth => {
            settings.editor.tab_width = number(settings.editor.tab_width as u64).min(255) as u8;
        }
        Field::TextSize => {
            settings.appearance.text_size =
                number(settings.appearance.text_size as u64).min(1000) as u16;
        }
        Field::UiSize => {
            settings.appearance.ui_size =
                number(settings.appearance.ui_size as u64).min(1000) as u16;
        }
        Field::MaxMb => settings.highlighting.max_mb = number(settings.highlighting.max_mb),
        Field::IndentSize => {
            settings.formatting.indent_size =
                number(settings.formatting.indent_size as u64).min(255) as u8;
        }
        Field::ThemeName => {
            let name = text.trim().to_string();
            if name.is_empty() {
                return;
            }
            let was = settings.appearance.theme.clone();
            if let Some(theme) = editing_mut(settings) {
                theme.name = name.clone();
            }
            if settings.appearance.theme == was {
                settings.appearance.theme = name;
            }
        }
        Field::ThemeSeed(n) => {
            if let Some(theme) = editing_mut(settings) {
                theme.set_seed(n, text.trim().to_string());
            }
        }
        _ => {}
    }
}

/// The custom theme the appearance page is editing.
fn editing_mut(settings: &mut Settings) -> Option<&mut CustomTheme> {
    let name = settings.appearance.theme.clone();
    settings
        .appearance
        .custom_themes
        .iter_mut()
        .find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use denise::theme;

    fn form(ui: &mut Ui<FormMsg>, settings: &Settings) -> Form {
        Form::build_in(ui, settings, None, 1.0, TextStyle::built_in(13)).expect("the form")
    }

    fn tree() -> Ui<FormMsg> {
        Ui::new(crate::settings_window::SIZE, theme::DARK)
    }

    /// Every section has rows, and nothing typed is lost walking between them.
    #[test]
    fn every_section_has_rows_and_the_draft_survives_them() {
        let mut ui = tree();
        let mut form = form(&mut ui, &Settings::default());
        for name in SECTIONS {
            assert!(form.show_section(name, &mut ui), "{name}");
            assert!(!form.controls.is_empty(), "{name} has no rows");
        }
        assert!(!form.show_section("Nothing Like It", &mut ui));
        assert_eq!(form.draft(), &Settings::default());
    }

    /// A control changed in the tree is the draft changed: the dialog reads
    /// what is on screen rather than being told about it.
    #[test]
    fn a_checkbox_ticked_is_the_setting_changed() {
        let mut ui = tree();
        let mut form = form(&mut ui, &Settings::default());
        let node = form
            .controls
            .iter()
            .find(|c| c.field == Field::ReopenTabs)
            .expect("the row")
            .node;
        ui.widget_mut::<Checkbox<FormMsg>>(node)
            .expect("checkbox")
            .set_checked(false);
        assert_eq!(form.handle(FormMsg::Ticked(false), &mut ui), None);
        assert!(!form.draft().general.reopen_tabs);
    }

    /// A new theme is a theme of the file's own, chosen, and editable where a
    /// built-in one is not.
    #[test]
    fn a_new_theme_is_made_chosen_and_editable() {
        let mut ui = tree();
        let mut form = form(&mut ui, &Settings::default());
        form.show_section("Appearance", &mut ui);
        assert_eq!(form.editing_theme(), None, "dark is built in");
        form.handle(FormMsg::Button(Action::NewTheme), &mut ui);
        assert_eq!(form.draft().appearance.theme, "custom");
        assert_eq!(form.editing_theme(), Some(0));
        form.handle(FormMsg::Button(Action::NewTheme), &mut ui);
        assert_eq!(
            form.draft().appearance.theme,
            "custom 2",
            "a name of its own"
        );
        assert_eq!(form.draft().appearance.custom_themes.len(), 2);
        form.handle(FormMsg::Button(Action::DeleteTheme), &mut ui);
        assert_eq!(form.draft().appearance.custom_themes.len(), 1);
        assert_eq!(
            form.draft().appearance.theme,
            "dark",
            "back to a built-in one"
        );
    }

    /// A dropdown opens over the dialog, and what is chosen in it is the
    /// setting it is for.
    #[test]
    fn a_dropdown_opens_and_what_is_chosen_is_the_setting() {
        let mut ui = tree();
        let mut form = form(&mut ui, &Settings::default());
        form.show_section("Formatting", &mut ui);
        let at = form
            .controls
            .iter()
            .position(|c| c.field == Field::Newline)
            .expect("the row");
        form.handle(FormMsg::OpenSelect(at), &mut ui);
        assert!(ui.popup_open(), "the list is open over the dialog");
        assert_eq!(form.open, Some(at));

        let crlf = NewlineStyle::ALL
            .iter()
            .position(|n| *n == NewlineStyle::CrLf)
            .expect("a row for it");
        form.handle(FormMsg::Chose(crlf), &mut ui);
        assert!(!ui.popup_open(), "choosing closes it");
        assert_eq!(form.draft().formatting.newline, NewlineStyle::CrLf);
    }

    /// The faces offered are the ones that suit the text, then a heading, then
    /// every face installed — and the heading is not a face.
    #[test]
    fn the_faces_offered_are_the_suitable_ones_and_then_all_of_them() {
        let mut ui = tree();
        let form = form(&mut ui, &Settings::default());
        let options = form.font_options(fonts::MONO, None);
        assert_eq!(options.first().map(String::as_str), Some(AUTOMATIC));
        let Some(heading) = options.iter().position(|o| o == EVERY_FACE) else {
            return; // a machine with no fonts at all has nothing to list
        };
        assert!(heading > 1, "the faces squint looks for come first");
        let all = &options[heading + 1..];
        assert!(!all.is_empty());
        let mut sorted = all.to_vec();
        sorted.sort_by_key(|name| name.to_lowercase());
        assert_eq!(all, sorted, "in order");

        let mut settings = Settings::default();
        set_choice(&mut settings, Field::TextFont, EVERY_FACE);
        assert_eq!(settings.appearance.text_font, None, "a heading, not a face");
        set_choice(&mut settings, Field::UiFont, EVERY_FACE);
        assert_eq!(settings.appearance.ui_font, None);
    }

    /// The faces drop out of the row that is for them, and what is chosen
    /// there is the face that is drawn in — separately for the text and for
    /// the chrome.
    #[test]
    fn the_text_and_the_chrome_take_their_faces_separately() {
        let mut ui = tree();
        let mut form = form(&mut ui, &Settings::default());
        form.show_section("Appearance", &mut ui);
        for (field, face) in [(Field::TextFont, 1usize), (Field::UiFont, 1usize)] {
            let at = form
                .controls
                .iter()
                .position(|c| c.field == field)
                .expect("the row");
            let offered = match &form.controls[at].kind {
                Kind::Select(options) => options.clone(),
                other => panic!("{other:?} is not a dropdown"),
            };
            // Longer than any screen would drop out whole, so it scrolls.
            assert!(offered.len() > 1);
            form.handle(FormMsg::OpenSelect(at), &mut ui);
            assert!(ui.popup_open(), "the faces are listed");
            form.handle(FormMsg::Chose(face), &mut ui);
            assert!(!ui.popup_open());
            let chosen = Some(offered[face].clone());
            match field {
                Field::TextFont => assert_eq!(form.draft().appearance.text_font, chosen),
                _ => assert_eq!(form.draft().appearance.ui_font, chosen),
            }
        }
        // One each, and neither followed the other.
        let appearance = &form.draft().appearance;
        assert!(appearance.text_font.is_some() && appearance.ui_font.is_some());
    }

    /// The buttons say what the window is to do.
    #[test]
    fn the_buttons_ask_the_window_for_what_they_say() {
        let mut ui = tree();
        let mut form = form(&mut ui, &Settings::default());
        for (action, outcome) in [
            (Action::Save, Some(Outcome::Save)),
            (Action::Apply, Some(Outcome::Apply)),
            (Action::Cancel, Some(Outcome::Close)),
            (Action::EditFile, Some(Outcome::EditFile)),
            (Action::Restore, None),
        ] {
            assert_eq!(form.handle(FormMsg::Button(action), &mut ui), outcome);
        }
    }

    #[test]
    fn a_colour_is_written_the_way_the_file_holds_it() {
        assert_eq!(hex_of(Color::rgb(30, 30, 46)), "#1E1E2E");
    }

    /// Every kind of control writes what it says into the draft.
    #[test]
    fn a_control_writes_the_setting_it_is_for() {
        let mut settings = Settings::default();
        set_check(&mut settings, Field::ReopenTabs, false);
        assert!(!settings.general.reopen_tabs);
        set_choice(&mut settings, Field::Indent, "tabs");
        assert_eq!(settings.formatting.indent, IndentStyle::Tabs);
        set_text(&mut settings, Field::TabWidth, "8".into());
        assert_eq!(settings.editor.tab_width, 8);
        set_text(&mut settings, Field::TabWidth, "not a number".into());
        assert_eq!(settings.editor.tab_width, 8, "kept, not zeroed");
        set_choice(&mut settings, Field::TextFont, AUTOMATIC);
        assert_eq!(settings.appearance.text_font, None);
        set_choice(&mut settings, Field::TextFont, "Menlo");
        assert_eq!(settings.appearance.text_font.as_deref(), Some("Menlo"));
    }

    /// Renaming the theme being edited keeps it the chosen one.
    #[test]
    fn renaming_the_theme_being_edited_keeps_it_chosen() {
        let mut settings = Settings::default();
        settings.appearance.custom_themes.push(CustomTheme {
            name: "custom".into(),
            ..CustomTheme::default()
        });
        settings.appearance.theme = "custom".into();
        set_text(&mut settings, Field::ThemeName, "midnight".into());
        assert_eq!(settings.appearance.theme, "midnight");
        assert_eq!(settings.appearance.custom_themes[0].name, "midnight");
        assert_eq!(settings.theme().name, "midnight");
    }
}
