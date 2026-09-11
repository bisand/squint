//! The window: the menus, a text area over the file, and a status line under
//! it.
//!
//! Work that walks the file — building the line index, finding text — is
//! done here in slices between frames rather than on a thread: the document
//! lives inside the widget, a slice is a few milliseconds, and the window
//! keeps drawing and taking keys while the status line counts up.
//!
//! The menus are one list of commands (see [`menu`](crate::menu)), drawn by the
//! system's menu bar on macOS and by DeniseUI's along the top of the window
//! everywhere else. A command comes the same way from either, or from its
//! keys, and [`App::run`] does it.

use crate::document::FileDocument;
use crate::fonts;
use crate::menu::{self, Command, State};
#[cfg(target_os = "macos")]
use crate::native_menu::NativeMenu;
use crate::recent::Recent;
use denise::{
    BufferAge, DamageTracker, ElementState, Frame, InputEvent, KeyCode, Modifiers, Pen, Rect, Size,
    theme,
};
use denise_text::TextStyle;
use denise_ui::widgets::{
    ClipboardRequest, Label, MenuBar, MenuEvent, TextArea, TextInput, open_menu, shortcut,
};
use denise_ui::{Anchors, NodeId, Ui};
use denise_winit::{DeniseApp, Present, WindowConfig};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use squint_core::editorconfig::{self, Properties};
use squint_core::format::{self, Format, Kind, Style};
use squint_core::{Find, FindStep};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Bytes indexed per step: a few milliseconds from the page cache.
const INDEX_SLICE: usize = 8 * 1024 * 1024;

/// Bytes searched per step.
const FIND_SLICE: usize = 4 * 1024 * 1024;

/// Bytes formatted per step.
const FORMAT_SLICE: usize = 4 * 1024 * 1024;

/// How long one frame may spend walking the file, indexing or finding.
const SLICE_TIME: Duration = Duration::from_millis(6);

/// The text's size in logical pixels, and the sizes Zoom In and Zoom Out step
/// through.
const TEXT_SIZE: u16 = 13;
const TEXT_SIZES: &[u16] = &[8, 9, 10, 11, 12, 13, 14, 16, 18, 20, 24, 28, 32, 40, 48];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Changed,
    Clipboard(ClipboardRequest),
    /// Enter in the prompt's field.
    Submit,
    /// A title of the menu bar in the window was pressed.
    MenuTitle(usize),
    /// What the menu open from that bar did.
    Menu(MenuEvent),
}

/// Where the menus are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menus {
    /// In the system's menu bar: macOS.
    System,
    /// DeniseUI's, along the top of the window.
    Window,
    /// None: a snapshot of a window whose menus the system draws.
    Off,
}

impl Menus {
    /// The platform's own: the system's bar on macOS, the window's everywhere
    /// else. `SQUINT_MENU=window` puts them in the window on macOS too.
    pub fn for_platform() -> Self {
        if cfg!(target_os = "macos") && std::env::var("SQUINT_MENU").as_deref() != Ok("window") {
            Menus::System
        } else {
            Menus::Window
        }
    }
}

/// What the field in the status line's place is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    GoTo,
    Find,
}

/// A field in the status line's place, and a note beside it that says what
/// the last answer came to. The status line comes back when it closes.
struct Prompt {
    ask: Ask,
    field: NodeId,
    note: NodeId,
}

/// A find on its way through the file.
struct Search {
    find: Find,
    query: String,
    /// Where it matched and whether it wrapped, once it has, while the index
    /// catches up far enough to say which line that is.
    hit: Option<(u64, bool)>,
}

/// A format on its way into a new file.
struct Formatting {
    job: Format,
    /// The layout used and where it came from, for the status line.
    note: String,
    writer: BufWriter<File>,
    /// Where it is being written: beside `out`, renamed to it when complete,
    /// so a half-written file is never opened.
    part: PathBuf,
    out: PathBuf,
}

/// A menu open from the bar in the window.
struct OpenMenu {
    /// The command behind each row, numbered as a pick is.
    commands: Vec<Option<Command>>,
    /// What had the keyboard before the menu took it, to be given it back.
    focus: Option<NodeId>,
}

pub struct App {
    ui: Ui<Msg>,
    editor: NodeId,
    status: NodeId,
    /// DeniseUI's menu bar, when the menus are in the window.
    bar: Option<NodeId>,
    open_menu: Option<OpenMenu>,
    #[cfg(target_os = "macos")]
    native: Option<NativeMenu>,
    /// What the system's menus were last brought up to date with.
    #[cfg(target_os = "macos")]
    shown: State,
    recent: Recent,
    prompt: Option<Prompt>,
    search: Option<Search>,
    formatting: Option<Formatting>,
    /// What the file looks like it could be formatted as, decided when it
    /// opens.
    format_kind: Option<Kind>,
    /// The last thing searched for, so ⌘G and F3 have something to find with
    /// the field closed, and the field opens holding it.
    last_query: String,
    /// The query the editor is marking on screen, so it is only reached —
    /// and so repainted — when that changes.
    highlighted: String,
    /// The face the text is drawn in, and its size in logical pixels.
    mono: TextStyle,
    text_size: u16,
    read_only: bool,
    scale: f32,
    title: String,
    /// What the status line says on the right: the last thing that happened.
    notice: String,
    clipboard: Option<arboard::Clipboard>,
    started: Instant,
    exit: bool,
}

impl App {
    pub fn config(present: Present) -> WindowConfig {
        WindowConfig {
            title: "squint".into(),
            size: Size::new(1000, 700),
            present,
            ..WindowConfig::default()
        }
    }

    pub fn new(
        size: Size,
        scale: f32,
        path: Option<&Path>,
        menus: Menus,
        mut recent: Recent,
    ) -> Self {
        let px = |v: f32| (v * scale + 0.5) as u16;
        let s = |v: i32| (v as f32 * scale + 0.5) as i32;
        let mut ui: Ui<Msg> = Ui::new(size, theme::DARK.scaled(scale));
        ui.show_cursor(false);
        let chrome = match fonts::load(fonts::UI) {
            Some((_, source)) => {
                let id = ui.add_font(source);
                ui.set_default_font(id);
                TextStyle {
                    font: id,
                    size_px: px(13.0),
                }
            }
            None => TextStyle::built_in(px(13.0)),
        };
        let mono = match fonts::load(fonts::MONO) {
            Some((_, source)) => TextStyle {
                font: ui.add_font(source),
                size_px: px(TEXT_SIZE as f32),
            },
            None => TextStyle::built_in(px(TEXT_SIZE as f32)),
        };

        let (doc, mut notice) = match path {
            Some(path) => match FileDocument::open(path) {
                Ok(doc) => {
                    recent.add(path);
                    (doc, String::new())
                }
                Err(e) => (FileDocument::empty(), format!("{}: {e}", path.display())),
            },
            None => (FileDocument::empty(), String::new()),
        };
        let title = match doc.path() {
            Some(p) => format!("{} — squint", name_of(p)),
            None => "squint".into(),
        };
        if notice.is_empty() && doc.path().is_none() {
            notice = format!("no file: {} opens one", shortcut("Cmd+O"));
        }
        // Tab stops as the file's project sets them, or every four columns.
        let tab_width = path
            .map(editorconfig::properties_for)
            .and_then(|props| props.tab_width())
            .unwrap_or(4);
        let format_kind = format::detect(doc.path(), &doc.head(8192));

        let root = ui.root();
        let (w, h) = (size.width as i32, size.height as i32);
        let status_h = s(24);
        let bar = (menus == Menus::Window).then(|| {
            let labels: Vec<String> = menu::titles(&State::default(), false)
                .into_iter()
                .map(|t| t.label)
                .collect();
            let widget = MenuBar::new(labels, Msg::MenuTitle).with_style(chrome);
            let theme = *ui.theme();
            let height = widget.preferred_height(&theme, ui.text_mut());
            let id = ui
                .add(root, widget, Rect::new(0, 0, w, height))
                .expect("menu bar");
            ui.set_anchors(id, TOP_ROW);
            (id, height)
        });
        let bar_h = bar.map_or(0, |(_, height)| height);
        let editor = ui
            .add(
                root,
                TextArea::new(doc)
                    .with_style(mono)
                    .with_tab_width(tab_width)
                    .with_change(Msg::Changed)
                    .with_clipboard(Msg::Clipboard),
                Rect::new(0, bar_h, w, h - status_h - bar_h),
            )
            .expect("editor");
        ui.set_anchors(editor, ALL_SIDES);
        let status = ui
            .add(
                root,
                Label::new("").with_size(px(11.0)),
                Rect::new(s(8), h - status_h, w - s(16), status_h),
            )
            .expect("status");
        ui.set_anchors(status, BOTTOM_ROW);
        ui.focus(Some(editor));

        let mut app = Self {
            ui,
            editor,
            status,
            bar: bar.map(|(id, _)| id),
            open_menu: None,
            #[cfg(target_os = "macos")]
            native: None,
            #[cfg(target_os = "macos")]
            shown: State::default(),
            recent,
            prompt: None,
            search: None,
            formatting: None,
            format_kind,
            last_query: String::new(),
            highlighted: String::new(),
            mono,
            text_size: TEXT_SIZE,
            read_only: false,
            scale,
            title,
            notice,
            clipboard: arboard::Clipboard::new().ok(),
            started: Instant::now(),
            exit: false,
        };
        #[cfg(target_os = "macos")]
        if menus == Menus::System {
            let state = app.menu_state();
            app.native = Some(NativeMenu::new(&menu::titles(&state, true)));
            app.shown = state;
        }
        app.refresh_status();
        app
    }

    /// Indexes the whole file now. For a snapshot, which has no frames to
    /// spread it over.
    pub fn index_all(&mut self) {
        while !self.editor_ref().document().is_indexed() {
            self.pump_index();
        }
    }

    /// Draws the window into `frame`, without an event loop.
    pub fn paint_into(&mut self, frame: &mut Frame<'_>) {
        self.ui.paint(frame);
    }

    /// Jumps to 1-based `line`, as the go-to-line field would.
    pub fn go_to_line(&mut self, line: usize) {
        self.editor().go_to(line.saturating_sub(1));
        self.say(format!("line {line}"));
    }

    /// Opens the find field holding `query`, finds forwards from the caret
    /// and waits for the answer. For a snapshot, which has no frames to
    /// spread the search over; the file must already be indexed.
    pub fn find_now(&mut self, query: &str) {
        self.open_prompt(Ask::Find);
        if let Some(field) = self.prompt.as_ref().map(|p| p.field)
            && let Some(input) = self.ui.widget_mut::<TextInput<Msg>>(field)
        {
            input.set_text(query);
        }
        self.sync_highlight();
        self.find(true);
        while self.search.is_some() {
            self.pump_find();
        }
    }

    /// Formats the file as ⇧⌘F does and waits for the result to open. For a
    /// snapshot, which has no frames to spread the work over.
    pub fn format_now(&mut self) {
        self.start_format();
        while self.formatting.is_some() {
            self.pump_format();
        }
    }

    /// Loads the grammars, decides on one and parses the file from the top,
    /// waiting for all of it. For a snapshot, which has no frames to spread
    /// the work over; the file must already be indexed.
    pub fn highlight_now(&mut self) {
        let head = self.editor_ref().document().head(8192);
        let path = self.editor_ref().document().path().map(Path::to_path_buf);
        if squint_core::syntax::worth_loading(path.as_deref(), &head) {
            squint_core::syntax::load();
        }
        self.editor().document_mut().decide_syntax();
        while self.editor_ref().document().highlighting() {
            self.pump_syntax();
        }
        self.refresh_status();
    }

    /// Opens the menu titled `label` from the bar in the window, as a press
    /// on its title would. For a snapshot. Whether there was one to open.
    pub fn open_menu_titled(&mut self, label: &str) -> bool {
        let index = self
            .bar
            .and_then(|bar| self.ui.widget::<MenuBar<Msg>>(bar))
            .and_then(|bar| {
                bar.titles()
                    .iter()
                    .position(|t| t.eq_ignore_ascii_case(label))
            });
        let Some(index) = index else {
            return false;
        };
        self.open_title(index);
        self.open_menu.is_some()
    }

    fn editor(&mut self) -> &mut TextArea<Msg, FileDocument> {
        self.ui
            .widget_mut::<TextArea<Msg, FileDocument>>(self.editor)
            .expect("editor")
    }

    fn editor_ref(&self) -> &TextArea<Msg, FileDocument> {
        self.ui
            .widget::<TextArea<Msg, FileDocument>>(self.editor)
            .expect("editor")
    }

    /// Indexes for a few milliseconds, if there is indexing left to do.
    fn pump_index(&mut self) {
        if self.editor_ref().document().is_indexed() {
            return;
        }
        let deadline = Instant::now() + SLICE_TIME;
        let doc = self.editor().document_mut();
        while !doc.index_step(INDEX_SLICE) && Instant::now() < deadline {}
        self.refresh_status();
    }

    /// Decides on a grammar once the grammars have loaded, and parses on from
    /// the top for a few milliseconds while there is parsing left. Reaching
    /// the document repaints the editor, which is what puts guessed colours
    /// right as the parse passes them.
    fn pump_syntax(&mut self) {
        let (undecided, busy, indexed) = {
            let doc = self.editor_ref().document();
            (doc.syntax_undecided(), doc.highlighting(), doc.is_indexed())
        };
        if undecided && self.editor().document_mut().decide_syntax() {
            self.refresh_status();
        }
        if busy && indexed {
            let deadline = Instant::now() + SLICE_TIME;
            self.editor().document_mut().highlight_until(deadline);
        }
    }

    // ---- the menus ----------------------------------------------------------

    /// What the menus should show now.
    fn menu_state(&self) -> State {
        let (has_file, modified) = {
            let doc = self.editor_ref().document();
            (doc.path().is_some(), doc.is_modified())
        };
        State {
            has_file,
            modified,
            can_format: self.format_kind.is_some(),
            read_only: self.read_only,
            line_numbers: self.editor_ref().gutter(),
            recent: self.recent.paths().to_vec(),
        }
    }

    /// Opens menu `index` of the bar in the window, closing any other.
    fn open_title(&mut self, index: usize) {
        let Some(bar) = self.bar else {
            return;
        };
        let focus = match self.open_menu.take() {
            Some(open) => {
                self.ui.close_popup();
                open.focus
            }
            None => self.ui.focused(),
        };
        let titles = menu::titles(&self.menu_state(), false);
        let Some(title) = titles.get(index) else {
            return;
        };
        let (items, commands) = menu::rows(&title.entries);
        if open_menu(&mut self.ui, bar, index, &items, Msg::Menu).is_some() {
            self.open_menu = Some(OpenMenu { commands, focus });
            self.light_title(Some(index));
        } else {
            self.ui.focus(focus);
            self.light_title(None);
        }
    }

    fn menu_event(&mut self, event: MenuEvent) {
        match event {
            MenuEvent::Title(index) => self.open_title(index),
            MenuEvent::Dismissed => self.close_menu(),
            MenuEvent::Picked(row) => {
                let command = self
                    .open_menu
                    .as_ref()
                    .and_then(|open| open.commands.get(row).copied().flatten());
                self.close_menu();
                if let Some(command) = command {
                    self.run(command);
                }
            }
        }
    }

    /// Closes the menu open from the bar, and gives the keyboard back.
    fn close_menu(&mut self) {
        if let Some(open) = self.open_menu.take() {
            self.ui.close_popup();
            self.ui.focus(open.focus);
        }
        self.light_title(None);
    }

    /// Escape closes a menu inside the tree without a word to anybody; the
    /// bar's title is put out and the keyboard given back once it has.
    fn settle_menu(&mut self) {
        if self.open_menu.is_some() && !self.ui.popup_open() {
            self.close_menu();
        }
    }

    fn light_title(&mut self, index: Option<usize>) {
        if let Some(bar) = self.bar
            && let Some(bar) = self.ui.widget_mut::<MenuBar<Msg>>(bar)
        {
            bar.set_open(index);
        }
    }

    /// Brings the system's menus up to date with what is true now, when that
    /// has changed.
    #[cfg(target_os = "macos")]
    fn sync_menus(&mut self) {
        if self.native.is_none() {
            return;
        }
        let state = self.menu_state();
        if state != self.shown {
            let titles = menu::titles(&state, true);
            if let Some(native) = &mut self.native {
                native.sync(&titles);
            }
            self.shown = state;
        }
    }

    /// Does what a menu row, or its keys, asks for.
    fn run(&mut self, command: Command) {
        match command {
            Command::New => {
                if self.may_discard() {
                    self.load(FileDocument::empty());
                    self.say("new file".into());
                }
            }
            Command::Open => {
                if self.may_discard()
                    && let Some(path) = self.pick_open()
                {
                    self.open_path(&path);
                }
            }
            Command::OpenRecent(n) => {
                if let Some(path) = self.recent.paths().get(n).cloned()
                    && self.may_discard()
                {
                    self.open_path(&path);
                }
            }
            Command::ClearRecent => self.recent.clear(),
            Command::Close | Command::Quit => {
                if self.may_discard() {
                    self.exit = true;
                }
            }
            Command::Save => self.save(),
            Command::SaveAs => self.save_as(),
            Command::Revert => self.revert(),
            Command::Undo
            | Command::Redo
            | Command::Cut
            | Command::Copy
            | Command::Paste
            | Command::SelectAll => self.press(command),
            Command::Find => self.open_prompt(Ask::Find),
            Command::FindNext => self.find(true),
            Command::FindPrevious => self.find(false),
            Command::GoToLine => self.open_prompt(Ask::GoTo),
            Command::ZoomIn => self.zoom(1),
            Command::ZoomOut => self.zoom(-1),
            Command::ActualSize => self.zoom(0),
            Command::LineNumbers => {
                let on = !self.editor_ref().gutter();
                self.editor().set_gutter(on);
            }
            Command::Format => self.start_format(),
            Command::ReadOnly => {
                self.read_only = !self.read_only;
                let read_only = self.read_only;
                self.editor().set_read_only(read_only);
                self.say(if read_only { "read only" } else { "editable" }.into());
            }
            Command::CopyPath => self.copy_path(),
            Command::Reveal => {
                let path = self.editor_ref().document().path().map(Path::to_path_buf);
                if let Some(path) = path
                    && let Err(e) = reveal(&path)
                {
                    self.say(format!("showing {}: {e}", path.display()));
                }
            }
            Command::Help => self.open_url(&format!("{}#readme", menu::HOME)),
            Command::ReportIssue => self.open_url(menu::ISSUES),
            Command::About => {
                MessageDialog::new()
                    .set_level(MessageLevel::Info)
                    .set_title("About squint")
                    .set_description(format!(
                        "squint {}\n\n{}\n\n{}",
                        env!("CARGO_PKG_VERSION"),
                        env!("CARGO_PKG_DESCRIPTION"),
                        menu::HOME
                    ))
                    .set_buttons(MessageButtons::Ok)
                    .show();
            }
        }
    }

    /// Does an edit by pressing its keys, so it happens exactly as they make
    /// it happen, to whatever has the keyboard.
    fn press(&mut self, command: Command) {
        let Some((code, modifiers)) = menu::edit_keys(command) else {
            return;
        };
        let key = |state| InputEvent::Key {
            code,
            state,
            repeat: false,
            modifiers,
        };
        self.ui
            .handle(&[key(ElementState::Down), key(ElementState::Up)]);
    }

    /// Acts on what the tree and the system's menus have said, until they
    /// have nothing more to say: a command can make the tree say something,
    /// as an edit pressed into the text area does. Whether anything was said.
    fn drain(&mut self) -> bool {
        let mut acted = false;
        // A command answered by another is fine; one that never stops is a
        // bug, and should not also hang the window.
        for _ in 0..8 {
            let messages: Vec<Msg> = self.ui.drain_messages().collect();
            #[cfg(target_os = "macos")]
            let chosen = self
                .native
                .as_ref()
                .map(NativeMenu::chosen)
                .unwrap_or_default();
            #[cfg(not(target_os = "macos"))]
            let chosen: Vec<Command> = Vec::new();
            if messages.is_empty() && chosen.is_empty() {
                break;
            }
            acted = true;
            for msg in messages {
                self.handle(msg);
            }
            for command in chosen {
                self.run(command);
            }
        }
        acted
    }

    // ---- files --------------------------------------------------------------

    /// Whether the document may be put away: it has no unsaved changes, or
    /// the user said to save them and they are saved, or to lose them.
    fn may_discard(&mut self) -> bool {
        let (modified, name) = {
            let doc = self.editor_ref().document();
            (
                doc.is_modified(),
                doc.path().map_or_else(|| "Untitled".to_string(), name_of),
            )
        };
        if !modified {
            return true;
        }
        let answer = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("squint")
            .set_description(format!(
                "Do you want to save the changes you made to {name}?\n\n\
                 Your changes will be lost if you don't save them."
            ))
            .set_buttons(MessageButtons::YesNoCancelCustom(
                "Save".into(),
                "Don't Save".into(),
                "Cancel".into(),
            ))
            .show();
        match answer {
            MessageDialogResult::Yes => {}
            MessageDialogResult::Custom(button) if button == "Save" => {}
            MessageDialogResult::No => return true,
            MessageDialogResult::Custom(button) if button == "Don't Save" => return true,
            _ => return false,
        }
        self.save();
        !self.editor_ref().document().is_modified()
    }

    fn pick_open(&self) -> Option<PathBuf> {
        let mut dialog = FileDialog::new().set_title("Open");
        if let Some(dir) = self.editor_ref().document().path().and_then(Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_file()
    }

    /// Opens `path` in place of the document. Whether it opened.
    fn open_path(&mut self, path: &Path) -> bool {
        match FileDocument::open(path) {
            Ok(doc) => {
                self.load(doc);
                self.recent.add(path);
                self.say(String::new());
                true
            }
            Err(e) => {
                if !path.exists() {
                    self.recent.remove(path);
                }
                self.say(format!("{}: {e}", path.display()));
                false
            }
        }
    }

    /// Puts `doc` in the editor in place of what was there, and forgets what
    /// was known about that.
    fn load(&mut self, doc: FileDocument) {
        let path = doc.path().map(Path::to_path_buf);
        self.editor().set_document(doc);
        self.search = None;
        // The new document marks nothing yet; an open find field sets it
        // again on the next frame.
        self.highlighted.clear();
        self.named(path.as_deref());
    }

    /// Takes on what the document's file decides: the title, the tab stops,
    /// and whether it can be formatted.
    fn named(&mut self, path: Option<&Path>) {
        self.title = match path {
            Some(p) => format!("{} — squint", name_of(p)),
            None => "squint".into(),
        };
        let tab_width = path
            .map(editorconfig::properties_for)
            .and_then(|props| props.tab_width())
            .unwrap_or(4);
        self.editor().set_tab_width(tab_width);
        let head = self.editor_ref().document().head(8192);
        self.format_kind = format::detect(path, &head);
    }

    fn save(&mut self) {
        if self.editor_ref().document().path().is_none() {
            return self.save_as();
        }
        let result = self.editor().document_mut().save();
        self.say(match result {
            Ok(()) => "saved".into(),
            Err(e) => e,
        });
    }

    fn save_as(&mut self) {
        let current = self.editor_ref().document().path().map(Path::to_path_buf);
        let mut dialog = FileDialog::new().set_title("Save As");
        match &current {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    dialog = dialog.set_directory(dir);
                }
                dialog = dialog.set_file_name(name_of(path));
            }
            None => dialog = dialog.set_file_name("Untitled.txt"),
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        let result = self.editor().document_mut().save_as(&path);
        match result {
            Ok(()) => {
                self.recent.add(&path);
                self.named(Some(&path));
                self.say(format!("saved as {}", path.display()));
            }
            Err(e) => self.say(e),
        }
    }

    /// Throws the changes away and opens the file again, at the same line.
    fn revert(&mut self) {
        let (path, modified) = {
            let doc = self.editor_ref().document();
            (doc.path().map(Path::to_path_buf), doc.is_modified())
        };
        let Some(path) = path.filter(|_| modified) else {
            return;
        };
        let answer = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("squint")
            .set_description(format!(
                "Revert {} to the saved version?\n\nYour changes will be lost.",
                name_of(&path)
            ))
            .set_buttons(MessageButtons::OkCancelCustom(
                "Revert".into(),
                "Cancel".into(),
            ))
            .show();
        let revert = match answer {
            MessageDialogResult::Ok => true,
            MessageDialogResult::Custom(button) => button == "Revert",
            _ => false,
        };
        if !revert {
            return;
        }
        let line = self.editor().caret().line;
        if self.open_path(&path) {
            self.editor().go_to(line);
            self.say("reverted".into());
        }
    }

    fn copy_path(&mut self) {
        let Some(path) = self.editor_ref().document().path().map(Path::to_path_buf) else {
            return;
        };
        let text = path.display().to_string();
        let copied = self.clipboard.as_mut().map(|c| c.set_text(text.clone()));
        self.say(match copied {
            Some(Ok(())) => format!("copied {text}"),
            Some(Err(e)) => format!("clipboard: {e}"),
            None => "there is no clipboard to copy to".into(),
        });
    }

    fn open_url(&mut self, url: &str) {
        if let Err(e) = open_url(url) {
            self.say(format!("opening {url}: {e}"));
        }
    }

    // ---- the view -----------------------------------------------------------

    /// One size bigger for `1`, one smaller for `-1`, the usual for `0`.
    fn zoom(&mut self, step: i32) {
        let now = self.text_size;
        let size = match step {
            0 => Some(TEXT_SIZE),
            s if s > 0 => TEXT_SIZES.iter().copied().find(|&t| t > now),
            _ => TEXT_SIZES.iter().rev().copied().find(|&t| t < now),
        }
        .unwrap_or(now);
        self.text_size = size;
        let style = TextStyle {
            size_px: (size as f32 * self.scale + 0.5) as u16,
            ..self.mono
        };
        self.editor().set_style(style);
        self.say(format!("text size {size}"));
    }

    // ---- the prompt -------------------------------------------------------

    /// Opens a field in the status line's place, or refocuses the one open.
    fn open_prompt(&mut self, ask: Ask) {
        if let Some(prompt) = &self.prompt {
            if prompt.ask == ask {
                let field = prompt.field;
                self.ui.focus(Some(field));
                return;
            }
            self.close_prompt();
        }
        let Some(row) = self.ui.bounds(self.status) else {
            return;
        };
        let px = |v: f32| (v * self.scale + 0.5) as u16;
        let s = |v: i32| (v as f32 * self.scale + 0.5) as i32;
        let (hint, max_chars, text) = match ask {
            Ask::GoTo => (
                "Go to line — Enter to jump, Esc to close",
                12,
                String::new(),
            ),
            Ask::Find => (
                "Find — Enter for next, Shift+Enter for previous, Esc to close",
                512,
                self.last_query.clone(),
            ),
        };
        let mut input = TextInput::<Msg>::new()
            .with_placeholder(hint)
            .with_submit(Msg::Submit)
            .with_max_chars(max_chars)
            .with_size(px(11.0));
        input.set_text(text);
        let root = self.ui.root();
        let field_w = row.width.min(s(440));
        let field_rect = Rect::new(row.x, row.y + s(2), field_w, row.height - s(4));
        let Some(field) = self.ui.add(root, input, field_rect) else {
            return;
        };
        let gap = s(12);
        let note_rect = Rect::new(
            row.x + field_w + gap,
            row.y,
            (row.width - field_w - gap).max(0),
            row.height,
        );
        let Some(note) = self
            .ui
            .add(root, Label::new("").with_size(px(11.0)), note_rect)
        else {
            self.ui.remove(field);
            return;
        };
        self.ui.set_anchors(
            field,
            Anchors {
                left: true,
                top: false,
                right: false,
                bottom: true,
            },
        );
        self.ui.set_anchors(note, BOTTOM_ROW);
        self.ui.set_visible(self.status, false);
        self.ui.focus(Some(field));
        self.prompt = Some(Prompt { ask, field, note });
    }

    fn close_prompt(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            self.ui.remove(prompt.field);
            self.ui.remove(prompt.note);
            self.ui.set_visible(self.status, true);
            self.ui.focus(Some(self.editor));
            self.refresh_status();
        }
    }

    /// What is typed in the prompt's field.
    fn prompt_text(&self) -> String {
        self.prompt
            .as_ref()
            .and_then(|p| self.ui.widget::<TextInput<Msg>>(p.field))
            .map(|f| f.text().to_string())
            .unwrap_or_default()
    }

    fn submit(&mut self) {
        match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::GoTo) => self.go_to(),
            Some(Ask::Find) => self.find(true),
            None => {}
        }
    }

    /// Says what happened: in the note beside an open prompt, and in the
    /// status line either way.
    fn say(&mut self, text: String) {
        self.notice = text;
        if let Some(note) = self.prompt.as_ref().map(|p| p.note)
            && let Some(label) = self.ui.widget_mut::<Label>(note)
        {
            label.set_text(self.notice.clone());
        }
        self.refresh_status();
    }

    // ---- going to a line ----------------------------------------------------

    /// Enter in the go-to-line field: jump, or say why not.
    fn go_to(&mut self) {
        let text = self.prompt_text().trim().to_string();
        self.close_prompt();
        match text.parse::<usize>() {
            Ok(n) if n > 0 => self.go_to_line(n),
            _ => self.say(format!("not a line number: {text:?}")),
        }
    }

    // ---- finding ------------------------------------------------------------

    /// Starts a search from the caret: forwards from the end of the
    /// selection, backwards from its start, so a match that is already
    /// selected is stepped past rather than found again.
    fn find(&mut self, forward: bool) {
        let query = match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::Find) => self.prompt_text(),
            _ => self.last_query.clone(),
        };
        if query.is_empty() {
            self.open_prompt(Ask::Find);
            self.say("type something to find".into());
            return;
        }
        self.last_query = query.clone();
        let editor = self.editor();
        let from = match editor.selection() {
            Some((start, end)) => {
                if forward {
                    end
                } else {
                    start
                }
            }
            None => editor.caret(),
        };
        let doc = editor.document_mut();
        let Some(origin) = doc.offset_of(from) else {
            self.say("the caret's line has not been counted yet".into());
            return;
        };
        let find = doc.find(&query, origin, forward);
        self.search = Some(Search {
            find,
            query,
            hit: None,
        });
        self.pump_find();
    }

    /// Searches for a few milliseconds, and selects the match when there is
    /// one and the index can say which line it is on.
    fn pump_find(&mut self) {
        let Some(mut search) = self.search.take() else {
            return;
        };
        if search.hit.is_none() {
            let deadline = Instant::now() + SLICE_TIME;
            let doc = self.editor().document_mut();
            let mut missing = false;
            loop {
                match doc.find_step(&mut search.find, FIND_SLICE) {
                    FindStep::Pending if Instant::now() < deadline => {}
                    FindStep::Pending => break,
                    FindStep::Found { at, wrapped } => {
                        search.hit = Some((at, wrapped));
                        break;
                    }
                    FindStep::NotFound => {
                        missing = true;
                        break;
                    }
                }
            }
            if missing {
                self.say(format!("no match for {:?}", search.query));
                return;
            }
        }
        let Some((at, wrapped)) = search.hit else {
            let (done, total) = search.find.progress();
            let percent = done * 100 / total.max(1);
            self.say(format!("finding {:?}… {percent}%", search.query));
            self.search = Some(search);
            return;
        };
        // A match past what the index has scanned cannot be given a line
        // without counting there by hand, which on a big file is the slow
        // read the index exists to avoid. The index is fast; wait for it.
        if !self.editor_ref().document().is_indexed() {
            self.say("found — counting lines to reach it…".into());
            self.search = Some(search);
            return;
        }
        let end = at + search.query.len() as u64;
        let doc = self.editor().document_mut();
        let (from, to) = (doc.pos_of(at), doc.pos_of(end));
        match (from, to) {
            (Some(from), Some(to)) => {
                self.editor().select_range(from, to);
                let place = format!("line {}", from.line + 1);
                self.say(if wrapped {
                    format!("{place} — wrapped around")
                } else {
                    place
                });
            }
            _ => self.say(format!("found {:?} but could not place it", search.query)),
        }
    }

    // ---- formatting ---------------------------------------------------------

    /// Pretty-prints the file as JSON or XML into a new file, a slice per
    /// frame, and opens that in this one's place when it is complete.
    fn start_format(&mut self) {
        if self.formatting.is_some() {
            return;
        }
        let (modified, path, head) = {
            let doc = self.editor_ref().document();
            (
                doc.is_modified(),
                doc.path().map(Path::to_path_buf),
                doc.head(8192),
            )
        };
        if modified {
            self.say("save first: the formatted file opens in place of this one".into());
            return;
        }
        let Some(kind) = format::detect(path.as_deref(), &head) else {
            self.say("not JSON or XML: nothing to format".into());
            return;
        };
        let out = formatted_path(path.as_deref(), kind);
        let part = part_path(&out);
        let created = fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))
            .and_then(|()| File::create(&part));
        let file = match created {
            Ok(file) => file,
            Err(e) => {
                self.say(format!("formatting: {}: {e}", part.display()));
                return;
            }
        };
        // The layout is the source file's project's, looked up from where the
        // source is: the output lands in the temporary directory, where no
        // project's .editorconfig is.
        let props = path
            .as_deref()
            .map(editorconfig::properties_for)
            .unwrap_or_default();
        let style = Style::from_editorconfig(&props);
        let note = style_note(style, &props);
        let job = self.editor_ref().document().format(kind, style);
        self.formatting = Some(Formatting {
            job,
            note,
            writer: BufWriter::with_capacity(1 << 20, file),
            part,
            out,
        });
        self.pump_format();
    }

    /// Formats for a few milliseconds.
    fn pump_format(&mut self) {
        let Some(mut running) = self.formatting.take() else {
            return;
        };
        let deadline = Instant::now() + SLICE_TIME;
        let step = {
            let doc = self.editor_ref().document();
            loop {
                match doc.format_step(&mut running.job, FORMAT_SLICE, &mut running.writer) {
                    Ok(false) if Instant::now() < deadline => {}
                    other => break other,
                }
            }
        };
        match step {
            Ok(false) => {
                let (done, total) = running.job.progress();
                let percent = done * 100 / total.max(1);
                let kind = running.job.kind().name();
                self.say(format!("formatting as {kind}… {percent}%"));
                self.formatting = Some(running);
            }
            Ok(true) => self.finish_format(running),
            Err(e) => {
                let _ = fs::remove_file(&running.part);
                self.say(format!("formatting: {e}"));
            }
        }
    }

    /// Puts the finished file in place and opens it.
    fn finish_format(&mut self, running: Formatting) {
        let Formatting {
            job,
            note,
            writer,
            part,
            out,
        } = running;
        let saved = writer
            .into_inner()
            .map_err(|e| e.into_error())
            .and_then(|file| file.sync_all())
            .and_then(|()| fs::rename(&part, &out));
        if let Err(e) = saved {
            let _ = fs::remove_file(&part);
            self.say(format!("formatting: {e}"));
            return;
        }
        match FileDocument::open(&out) {
            Ok(doc) => {
                self.load(doc);
                let kind = job.kind().name();
                self.say(format!(
                    "formatted as {kind} with {note} into {}",
                    out.display()
                ));
            }
            Err(e) => self.say(format!("{}: {e}", out.display())),
        }
    }

    fn cancel_format(&mut self, why: &str) {
        if let Some(running) = self.formatting.take() {
            drop(running.writer);
            let _ = fs::remove_file(&running.part);
            self.say(format!("formatting stopped: {why}"));
        }
    }

    // ---- the rest -----------------------------------------------------------

    fn refresh_status(&mut self) {
        let editor = self.editor();
        let caret = editor.caret();
        let modified = editor.document().is_modified();
        let doc = editor.document_mut();
        let error = doc.take_error();
        let (scanned, total) = doc.index_progress();
        let lines = if doc.is_indexed() {
            format!("{} lines", line_count(doc))
        } else {
            let percent = if total == 0 {
                100
            } else {
                scanned * 100 / total
            };
            format!("counting… {percent}%")
        };
        let syntax = doc
            .syntax_name()
            .map(|name| format!("   {name}"))
            .unwrap_or_default();
        let held = doc.memory_bytes() / 1024;
        if let Some(error) = error {
            self.notice = error;
        }
        let mark = if modified { " •" } else { "" };
        let text = format!(
            "Ln {}, Col {}   {lines}{syntax}   {held} KB held{mark}   {}",
            caret.line + 1,
            caret.col + 1,
            self.notice
        );
        if let Some(label) = self.ui.widget_mut::<Label>(self.status) {
            label.set_text(text);
        }
        let base = self.title.trim_start_matches('•').trim_start().to_string();
        self.title = if modified {
            format!("• {base}")
        } else {
            base
        };
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            // What a search or a format has read moved with the edit.
            Msg::Changed => {
                self.search = None;
                self.cancel_format("the text changed");
            }
            Msg::Submit => self.submit(),
            Msg::Clipboard(ClipboardRequest::Copy(text) | ClipboardRequest::Cut(text)) => {
                if let Some(clip) = &mut self.clipboard
                    && let Err(e) = clip.set_text(text)
                {
                    self.notice = format!("clipboard: {e}");
                }
            }
            Msg::Clipboard(ClipboardRequest::Paste) => {
                let text = self.clipboard.as_mut().and_then(|c| c.get_text().ok());
                if let Some(text) = text {
                    self.editor().insert_text(&text);
                }
            }
            Msg::MenuTitle(index) => self.open_title(index),
            Msg::Menu(event) => self.menu_event(event),
        }
    }

    /// Marks every match of what is typed in the find field on the lines on
    /// screen, as it is typed, and nothing once the field closes.
    fn sync_highlight(&mut self) {
        let query = match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::Find) => self.prompt_text(),
            _ => String::new(),
        };
        if query != self.highlighted {
            self.editor().document_mut().set_highlight(&query);
            self.highlighted = query;
        }
    }

    /// Whether the find field has the keyboard.
    fn typing_a_find(&self) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|p| p.ask == Ask::Find && self.ui.focused() == Some(p.field))
    }
}

const ALL_SIDES: Anchors = Anchors {
    left: true,
    top: true,
    right: true,
    bottom: true,
};

const TOP_ROW: Anchors = Anchors {
    left: true,
    top: true,
    right: true,
    bottom: false,
};

const BOTTOM_ROW: Anchors = Anchors {
    left: true,
    top: false,
    right: true,
    bottom: true,
};

/// The document's line count, once it is known.
fn line_count(doc: &mut FileDocument) -> usize {
    use denise_ui::widgets::TextDocument;
    doc.line_count().unwrap_or(0)
}

/// Where a formatted copy of `path` goes: the system's temporary directory,
/// under the file's name with `-formatted` before the extension. It keeps the
/// extension that says what it is, and can never be the file it came from.
fn formatted_path(path: Option<&Path>, kind: Kind) -> PathBuf {
    let stem = path
        .and_then(Path::file_stem)
        .map_or_else(|| "untitled".into(), |s| s.to_string_lossy().into_owned());
    let ext = path.and_then(Path::extension).map_or_else(
        || match kind {
            Kind::Json => "json".to_string(),
            Kind::Xml => "xml".to_string(),
        },
        |e| e.to_string_lossy().into_owned(),
    );
    std::env::temp_dir()
        .join("squint")
        .join(format!("{stem}-formatted.{ext}"))
}

/// A style in a few words, and the `.editorconfig` it came from if one did:
/// `4 spaces, LF from /work/proj/.editorconfig`.
fn style_note(style: Style, props: &Properties) -> String {
    match props.sources().first() {
        Some(file) => format!("{} from {}", style.describe(), file.display()),
        None => style.describe(),
    }
}

fn part_path(out: &Path) -> PathBuf {
    let mut name = out.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Starts `program` and lets it get on with it, waited for on a thread of its
/// own so it does not linger as a zombie.
fn launch<S: AsRef<OsStr>>(program: &str, args: impl IntoIterator<Item = S>) -> io::Result<()> {
    let mut child = std::process::Command::new(program).args(args).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Opens `url` in the user's browser.
fn open_url(url: &str) -> io::Result<()> {
    if cfg!(target_os = "macos") {
        launch("open", [url])
    } else if cfg!(windows) {
        launch("explorer", [url])
    } else {
        launch("xdg-open", [url])
    }
}

/// Shows `path` in the platform's file manager: selected, where it can be.
fn reveal(path: &Path) -> io::Result<()> {
    if cfg!(target_os = "macos") {
        launch("open", [OsStr::new("-R"), path.as_os_str()])
    } else if cfg!(windows) {
        let mut select = OsString::from("/select,");
        select.push(path);
        launch("explorer", [select])
    } else {
        let dir = path.parent().unwrap_or(Path::new("/"));
        launch("xdg-open", [dir.as_os_str()])
    }
}

impl DeniseApp for App {
    fn update(&mut self, events: &[InputEvent], damage: &mut DamageTracker) {
        let mut forwarded: Vec<InputEvent> = Vec::with_capacity(events.len());
        for event in events {
            // An open menu has the keyboard, Escape included.
            let menu_up = self.ui.popup_open();
            match event {
                // Answered by `close_requested`, which could still say no.
                InputEvent::CloseRequested => continue,
                InputEvent::Key {
                    code: KeyCode::Escape,
                    state: ElementState::Down,
                    ..
                } if self.prompt.is_some() && !menu_up => {
                    self.close_prompt();
                    continue;
                }
                // Enter alone is the field's own submit, which finds forwards.
                InputEvent::Key {
                    code: KeyCode::Enter | KeyCode::NumpadEnter,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } if modifiers.contains(Modifiers::SHIFT) && self.typing_a_find() => {
                    self.find(false);
                    continue;
                }
                // F10 goes to the menu bar, as it does on Windows and on most
                // Linux desktops.
                InputEvent::Key {
                    code: KeyCode::F10,
                    state: ElementState::Down,
                    ..
                } if self.bar.is_some() && !menu_up => {
                    self.open_title(0);
                    continue;
                }
                InputEvent::Key {
                    code,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } if !menu_up => {
                    if let Some(command) = menu::shortcut(*code, *modifiers) {
                        self.run(command);
                        continue;
                    }
                }
                _ => {}
            }
            forwarded.push(event.clone());
        }
        self.ui.handle(&forwarded);
        self.ui.tick(self.started.elapsed().as_millis() as u64);
        self.settle_menu();
        let acted = self.drain();
        self.sync_highlight();
        self.pump_index();
        self.pump_find();
        self.pump_format();
        self.pump_syntax();
        #[cfg(target_os = "macos")]
        self.sync_menus();
        if !forwarded.is_empty() || acted {
            self.refresh_status();
        }
        if self.ui.needs_paint() {
            let pending = self.ui.pending_damage();
            if pending.is_empty() {
                damage.add_full();
            } else {
                for rect in pending {
                    damage.add(*rect);
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, _damage: &[Rect]) {
        self.ui.paint(frame);
        self.ui.presented();
    }

    fn paint(&mut self, pen: &mut Pen<'_>, age: BufferAge, _damage: &[Rect]) -> bool {
        self.ui.paint_with(pen, age);
        self.ui.presented();
        true
    }

    fn exit_requested(&self) -> bool {
        self.exit
    }

    /// The close button asks about unsaved changes first, and a Cancel keeps
    /// the window.
    fn close_requested(&mut self) -> bool {
        self.may_discard()
    }

    fn title(&self) -> Option<&str> {
        Some(&self.title)
    }

    fn next_frame_in(&self) -> Option<Duration> {
        let doc = self.editor_ref().document();
        if !doc.is_indexed()
            || doc.highlighting()
            || self.search.is_some()
            || self.formatting.is_some()
        {
            // Straight back: there is a slice of the file to walk.
            return Some(Duration::ZERO);
        }
        if doc.syntax_undecided() {
            // The grammars are loading on their thread; look again shortly.
            return Some(Duration::from_millis(16));
        }
        drop(doc);
        let now = self.started.elapsed().as_millis() as u64;
        self.ui
            .next_wake_ms()
            .map(|w| Duration::from_millis(w.saturating_sub(now)))
    }
}
