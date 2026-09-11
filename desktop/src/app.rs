//! The window: the menus, a row of tabs, the text of the tab in front, and a
//! status line under it.
//!
//! Work that walks the file — building the line index, finding text — is
//! done here in slices between frames rather than on a thread: the document
//! lives inside the widget, a slice is a few milliseconds, and the window
//! keeps drawing and taking keys while the status line counts up.
//!
//! Each tab is a text area of its own, all of them in the same place and only
//! the one in front shown. A tab behind does no work: its file is counted and
//! coloured when it comes to the front. What the tabs were — their files, the
//! names and colours they were given, the lines they were on — is kept when
//! squint closes, to open again when it starts; and every file is looked at
//! every couple of seconds, in case something else has changed it.
//!
//! The menus are one list of commands (see [`menu`](crate::menu)), drawn by the
//! system's menu bar on macOS and by DeniseUI's along the top of the window
//! everywhere else. A command comes the same way from either, from its keys,
//! or from the menu a right-clicked tab opens, and [`App::run`] does it.

use crate::config::Kept;
use crate::document::FileDocument;
use crate::fonts;
use crate::menu::{self, Command, State};
#[cfg(target_os = "macos")]
use crate::native_menu::NativeMenu;
use crate::recent::Recent;
use crate::session::{self, SavedTab, Session, TabColor};
use crate::settings::{self, Settings};
use crate::stamp::Stamp;
use crate::switcher::{Switcher, TabId};
use denise::{
    BufferAge, Color, DamageTracker, ElementState, Frame, InputEvent, KeyCode, Modifiers, Pen,
    Point, Rect, Size, theme,
};
use denise_text::TextStyle;
use denise_ui::widgets::{
    ClipboardRequest, Label, MenuBar, MenuEvent, TabEvent, Tabs, TextArea, TextDocument, TextInput,
    open_menu, open_menu_at, shortcut, tab_rect,
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

/// How often the tabs' files are looked at for changes made by something else.
const CHECK_FILES: Duration = Duration::from_secs(2);

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
    /// What a menu open from that bar, or from a tab, did.
    Menu(MenuEvent),
    /// What was done to the row of tabs.
    Tab(TabEvent),
    /// Enter in the field renaming a tab.
    Renamed,
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

/// What the window remembers between runs.
pub struct Remembered {
    pub recent: Recent,
    pub settings: Kept<Settings>,
    pub session: Kept<Session>,
}

impl Remembered {
    /// What the last run left.
    pub fn load() -> Self {
        Self {
            recent: Recent::load(),
            settings: Kept::load(settings::FILE),
            session: Kept::load(session::FILE),
        }
    }

    /// Nothing remembered, and nothing written: for a snapshot.
    pub fn in_memory() -> Self {
        Self {
            recent: Recent::in_memory(),
            settings: Kept::in_memory(Settings::default()),
            session: Kept::in_memory(Session::default()),
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

/// A find on its way through a file.
struct Search {
    /// The text area it is finding in, which need not stay in front.
    editor: NodeId,
    find: Find,
    query: String,
    /// Where it matched and whether it wrapped, once it has, while the index
    /// catches up far enough to say which line that is.
    hit: Option<(u64, bool)>,
}

/// A format on its way into a new file.
struct Formatting {
    /// The text area whose document is being formatted.
    editor: NodeId,
    job: Format,
    /// The layout used and where it came from, for the status line.
    note: String,
    writer: BufWriter<File>,
    /// Where it is being written: beside `out`, renamed to it when complete,
    /// so a half-written file is never opened.
    part: PathBuf,
    out: PathBuf,
}

/// A menu open from the bar in the window, or from a tab.
struct OpenMenu {
    /// The command behind each row, numbered as a pick is.
    commands: Vec<Option<Command>>,
    /// What had the keyboard before the menu took it, to be given it back.
    focus: Option<NodeId>,
}

/// One tab: a file, or none yet, in a text area of its own.
struct Tab {
    id: TabId,
    editor: NodeId,
    /// The file, while it has one.
    path: Option<PathBuf>,
    /// The name it was given; the file's name while it has none.
    name: Option<String>,
    color: Option<TabColor>,
    /// What the file looks like it could be formatted as, decided when it
    /// opens.
    format_kind: Option<Kind>,
    read_only: bool,
    /// The file as it was when squint last read or wrote it.
    stamp: Option<Stamp>,
    /// Asked to stop being told about changes to this file, until it is read
    /// again.
    ignore_changes: bool,
    /// A line to go to once the index has counted that far: where a reopened
    /// or reloaded tab was.
    pending_line: Option<usize>,
}

/// A field over a tab, renaming it.
struct Rename {
    tab: TabId,
    field: NodeId,
}

/// What to do about a file something else changed.
enum Change {
    Reload,
    Keep,
    Ignore,
}

pub struct App {
    ui: Ui<Msg>,
    tabs: Vec<Tab>,
    /// Where the tab in front is in `tabs`.
    active: usize,
    /// The row of tabs.
    strip: NodeId,
    status: NodeId,
    /// DeniseUI's menu bar, when the menus are in the window.
    bar: Option<NodeId>,
    open_menu: Option<OpenMenu>,
    #[cfg(target_os = "macos")]
    native: Option<NativeMenu>,
    /// What the system's menus were last brought up to date with.
    #[cfg(target_os = "macos")]
    shown: State,
    memory: Remembered,
    switcher: Switcher,
    next_id: TabId,
    rename: Option<Rename>,
    /// Where a tab's text starts, below the menu bar and the tabs.
    page_top: i32,
    status_h: i32,
    prompt: Option<Prompt>,
    search: Option<Search>,
    formatting: Option<Formatting>,
    /// The last thing searched for, so ⌘G and F3 have something to find with
    /// the field closed, and the field opens holding it.
    last_query: String,
    /// The text area being marked with the query in the find field, and the
    /// query, so it is only reached — and so repainted — when either changes.
    highlighted: Option<(NodeId, String)>,
    /// The face the tabs and their rename field are drawn in.
    small: TextStyle,
    /// The face the text is drawn in, and its size in logical pixels.
    mono: TextStyle,
    text_size: u16,
    gutter: bool,
    scale: f32,
    title: String,
    /// What the status line says on the right: the last thing that happened.
    notice: String,
    clipboard: Option<arboard::Clipboard>,
    started: Instant,
    last_check: Instant,
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
        memory: Remembered,
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
        let small = TextStyle {
            size_px: px(12.0),
            ..chrome
        };
        let mono = match fonts::load(fonts::MONO) {
            Some((_, source)) => TextStyle {
                font: ui.add_font(source),
                size_px: px(TEXT_SIZE as f32),
            },
            None => TextStyle::built_in(px(TEXT_SIZE as f32)),
        };

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
        let strip_widget = Tabs::with_events(Vec::<String>::new(), Msg::Tab)
            .with_close_buttons(true)
            .with_style(small);
        let strip_h = strip_widget.strip_height(ui.theme());
        let strip = ui
            .add(root, strip_widget, Rect::new(0, bar_h, w, strip_h))
            .expect("tabs");
        ui.set_anchors(strip, TOP_ROW);
        let status = ui
            .add(
                root,
                Label::new("").with_size(px(11.0)),
                Rect::new(s(8), h - status_h, w - s(16), status_h),
            )
            .expect("status");
        ui.set_anchors(status, BOTTOM_ROW);

        let mut app = Self {
            ui,
            tabs: Vec::new(),
            active: 0,
            strip,
            status,
            bar: bar.map(|(id, _)| id),
            open_menu: None,
            #[cfg(target_os = "macos")]
            native: None,
            #[cfg(target_os = "macos")]
            shown: State::default(),
            memory,
            switcher: Switcher::default(),
            next_id: 0,
            rename: None,
            page_top: bar_h + strip_h,
            status_h,
            prompt: None,
            search: None,
            formatting: None,
            last_query: String::new(),
            highlighted: None,
            small,
            mono,
            text_size: TEXT_SIZE,
            gutter: true,
            scale,
            title: "squint".into(),
            notice: String::new(),
            clipboard: arboard::Clipboard::new().ok(),
            started: Instant::now(),
            last_check: Instant::now(),
            exit: false,
        };
        let front = app.restore(path);
        app.show_tab(front, true);
        // The system's window tabs would take Ctrl+Tab from squint's own.
        #[cfg(target_os = "macos")]
        if menus != Menus::Off {
            crate::native_menu::forbid_window_tabs();
        }
        #[cfg(target_os = "macos")]
        if menus == Menus::System {
            let state = app.menu_state();
            app.native = Some(NativeMenu::new(&menu::titles(&state, true)));
            app.shown = state;
        }
        app.refresh_status();
        app
    }

    /// Opens the tabs the last run left, when the settings say to, and the
    /// file named when squint was started. Returns the tab to put in front.
    fn restore(&mut self, path: Option<&Path>) -> usize {
        let session = self.memory.session.get().clone();
        let mut front = None;
        if self.memory.settings.get().reopen_tabs {
            for (i, saved) in session.tabs.iter().enumerate() {
                // A file that has gone since is left out, quietly: it was
                // somebody's decision to remove it.
                let Ok(doc) = FileDocument::open(&saved.path) else {
                    continue;
                };
                let index = self.add_tab(doc);
                let tab = &mut self.tabs[index];
                tab.name = saved.name.clone();
                tab.color = saved.color;
                tab.pending_line = (saved.line > 0).then_some(saved.line);
                if i == session.active {
                    front = Some(index);
                }
            }
        }
        if let Some(path) = path {
            match self.tab_with(path) {
                Some(index) => front = Some(index),
                None => match FileDocument::open(path) {
                    Ok(doc) => {
                        front = Some(self.add_tab(doc));
                        self.memory.recent.add(path);
                    }
                    Err(e) => self.notice = format!("{}: {e}", path.display()),
                },
            }
        }
        if self.tabs.is_empty() {
            self.add_tab(FileDocument::empty());
            if self.notice.is_empty() {
                self.notice = format!("no file: {} opens one", shortcut("Cmd+O"));
            }
        }
        front.unwrap_or(0)
    }

    /// Indexes the whole file now. For a snapshot, which has no frames to
    /// spread it over.
    pub fn index_all(&mut self) {
        while !self.editor_ref().document().is_indexed() {
            self.pump_index();
        }
        self.settle_pending_line();
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
        let path = self.tabs[self.active].path.clone();
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

    // ---- tabs ---------------------------------------------------------------

    fn area(&self, editor: NodeId) -> Option<&TextArea<Msg, FileDocument>> {
        self.ui.widget::<TextArea<Msg, FileDocument>>(editor)
    }

    fn area_mut(&mut self, editor: NodeId) -> Option<&mut TextArea<Msg, FileDocument>> {
        self.ui.widget_mut::<TextArea<Msg, FileDocument>>(editor)
    }

    /// The text area of the tab in front.
    fn editor_id(&self) -> NodeId {
        self.tabs[self.active].editor
    }

    fn editor(&mut self) -> &mut TextArea<Msg, FileDocument> {
        let editor = self.editor_id();
        self.area_mut(editor).expect("editor")
    }

    fn editor_ref(&self) -> &TextArea<Msg, FileDocument> {
        self.area(self.editor_id()).expect("editor")
    }

    fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The tab that has `path` open.
    fn tab_with(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path))
    }

    fn is_modified(&self, index: usize) -> bool {
        self.tabs
            .get(index)
            .and_then(|t| self.area(t.editor))
            .is_some_and(|area| area.document().is_modified())
    }

    /// An untitled tab nobody has typed in: what a file opened replaces.
    fn is_blank(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|t| t.path.is_none()) && !self.is_modified(index)
    }

    /// What a tab is called: the name it was given, or its file's.
    fn tab_name(&self, index: usize) -> String {
        let Some(tab) = self.tabs.get(index) else {
            return String::new();
        };
        tab.name.clone().unwrap_or_else(|| {
            tab.path
                .as_deref()
                .map_or_else(|| "Untitled".to_string(), name_of)
        })
    }

    /// What a tab says: its name, marked while it has unsaved changes.
    fn tab_label(&self, index: usize) -> String {
        let mut label = self.tab_name(index);
        if self.is_modified(index) {
            label.push_str(" •");
        }
        label
    }

    /// Where a tab's text goes, in the window as it is now.
    fn page_rect(&self) -> Rect {
        let size = self.ui.size();
        let (w, h) = (size.width as i32, size.height as i32);
        Rect::new(
            0,
            self.page_top,
            w,
            (h - self.page_top - self.status_h).max(0),
        )
    }

    fn editor_style(&self) -> TextStyle {
        TextStyle {
            size_px: (self.text_size as f32 * self.scale + 0.5) as u16,
            ..self.mono
        }
    }

    /// Adds a tab for `doc` at the end of the row, behind the others. Returns
    /// where it is.
    fn add_tab(&mut self, doc: FileDocument) -> usize {
        let path = doc.path().map(Path::to_path_buf);
        let format_kind = format::detect(path.as_deref(), &doc.head(8192));
        let area = TextArea::new(doc)
            .with_style(self.editor_style())
            .with_tab_width(tab_width_for(path.as_deref()))
            .with_gutter(self.gutter)
            .with_change(Msg::Changed)
            .with_clipboard(Msg::Clipboard);
        let root = self.ui.root();
        let page = self.page_rect();
        let editor = self.ui.add(root, area, page).expect("editor");
        self.ui.set_anchors(editor, ALL_SIDES);
        self.ui.set_visible(editor, false);
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            editor,
            stamp: path.as_deref().and_then(Stamp::of),
            path,
            name: None,
            color: None,
            format_kind,
            read_only: false,
            ignore_changes: false,
            pending_line: None,
        });
        self.tabs.len() - 1
    }

    /// Brings tab `index` to the front. `remember` makes it the last tab for
    /// Ctrl+Tab, which a walk along the row with Ctrl held does not.
    fn show_tab(&mut self, index: usize, remember: bool) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (id, editor) = (tab.id, tab.editor);
        if let Some(old) = self.tabs.get(self.active).map(|t| t.editor)
            && old != editor
        {
            self.ui.set_visible(old, false);
        }
        self.ui.set_visible(editor, true);
        self.active = index;
        if remember {
            self.switcher.brought_forward(id);
        }
        if self.prompt.is_none() && self.rename.is_none() && self.open_menu.is_none() {
            self.ui.focus(Some(editor));
        }
        self.sync_strip();
        self.refresh_status();
    }

    /// The next tab along the row, or the one before, wrapping.
    fn step_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n < 2 {
            return;
        }
        let next = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
        self.show_tab(next, true);
    }

    /// Tab with Ctrl held. See [`Switcher`].
    fn cycle_tab(&mut self, backwards: bool) {
        let row: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        let front = self.tabs[self.active].id;
        if let Some(to) = self.switcher.tab_pressed(&row, front, backwards)
            && let Some(index) = self.index_of(to)
        {
            self.show_tab(index, false);
        }
    }

    fn ctrl_released(&mut self) {
        let front = self.tabs[self.active].id;
        self.switcher.released(front);
    }

    /// Takes tab `index` out, without asking anything, and brings forward the
    /// tab that was in front before it if it was in front.
    fn remove_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let front = index == self.active;
        let tab = self.tabs.remove(index);
        self.forget_editor(tab.editor);
        self.ui.remove(tab.editor);
        self.switcher.closed(tab.id);
        if let Some(rename) = &self.rename
            && rename.tab == tab.id
        {
            let field = rename.field;
            self.rename = None;
            self.ui.remove(field);
        }
        if self.tabs.is_empty() {
            self.active = 0;
            return;
        }
        if index < self.active {
            self.active -= 1;
        }
        if front {
            let row: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
            let next = self
                .switcher
                .last_in_front(&row)
                .and_then(|id| self.index_of(id))
                .unwrap_or(index.min(self.tabs.len() - 1));
            self.active = next;
            self.show_tab(next, true);
        }
    }

    /// Stops whatever was working on `editor`, which is going away or getting
    /// a different document.
    fn forget_editor(&mut self, editor: NodeId) {
        if self.search.as_ref().is_some_and(|s| s.editor == editor) {
            self.search = None;
        }
        if self.formatting.as_ref().is_some_and(|f| f.editor == editor) {
            self.cancel_format("its tab changed");
        }
        if self
            .highlighted
            .as_ref()
            .is_some_and(|(node, _)| *node == editor)
        {
            self.highlighted = None;
        }
    }

    /// Closes tab `index`, asking first about unsaved changes. The last tab
    /// closes the window when there is nothing in it, and otherwise leaves an
    /// empty one.
    fn close_tab(&mut self, index: usize) {
        if !self.may_discard_tab(index) {
            return;
        }
        if self.tabs.len() == 1 && self.is_blank(0) {
            self.exit = true;
            return;
        }
        self.remove_tab(index);
        if self.tabs.is_empty() {
            let blank = self.add_tab(FileDocument::empty());
            self.show_tab(blank, true);
        }
        self.sync_strip();
        self.remember_tabs();
    }

    fn close_other_tabs(&mut self) {
        let keep = self.tabs[self.active].id;
        let others: Vec<TabId> = self
            .tabs
            .iter()
            .map(|t| t.id)
            .filter(|&id| id != keep)
            .collect();
        for id in others {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            if !self.may_discard_tab(index) {
                break;
            }
            if let Some(index) = self.index_of(id) {
                self.remove_tab(index);
            }
        }
        if let Some(index) = self.index_of(keep) {
            self.show_tab(index, true);
        }
        self.sync_strip();
        self.remember_tabs();
    }

    /// Brings the row of tabs up to date: names, colours and which is in
    /// front. Reaching the strip repaints it, so it is only reached when
    /// something differs.
    fn sync_strip(&mut self) {
        let labels: Vec<String> = (0..self.tabs.len()).map(|i| self.tab_label(i)).collect();
        let colors: Vec<Option<Color>> = self
            .tabs
            .iter()
            .map(|t| t.color.map(TabColor::color))
            .collect();
        let active = self.active;
        let same = self
            .ui
            .widget::<Tabs<Msg>>(self.strip)
            .is_some_and(|strip| {
                strip.labels() == labels.as_slice()
                    && strip.colors() == colors.as_slice()
                    && strip.selected() == active
            });
        if same {
            return;
        }
        if let Some(strip) = self.ui.widget_mut::<Tabs<Msg>>(self.strip) {
            if strip.labels() != labels.as_slice() {
                strip.set_labels(labels);
            }
            if strip.colors() != colors.as_slice() {
                strip.set_colors(colors);
            }
            strip.set_selected(active);
        }
    }

    /// Writes down the tabs with files, for the next run.
    fn remember_tabs(&mut self) {
        let mut session = Session::default();
        for (index, tab) in self.tabs.iter().enumerate() {
            let Some(path) = tab.path.clone() else {
                continue;
            };
            if index == self.active {
                session.active = session.tabs.len();
            }
            let line = tab
                .pending_line
                .or_else(|| self.area(tab.editor).map(|area| area.caret().line))
                .unwrap_or(0);
            session.tabs.push(SavedTab {
                path,
                name: tab.name.clone(),
                color: tab.color,
                line,
            });
        }
        if session != *self.memory.session.get() {
            self.memory.session.set(session);
        }
    }

    fn tab_event(&mut self, event: TabEvent) {
        match event {
            TabEvent::Selected(index) => self.show_tab(index, true),
            TabEvent::Close(index) => self.close_tab(index),
            TabEvent::Moved { from, to } => {
                // The strip has moved its own tab already; the tabs here follow.
                if from < self.tabs.len() && to < self.tabs.len() {
                    let front = self.tabs[self.active].id;
                    let tab = self.tabs.remove(from);
                    self.tabs.insert(to, tab);
                    self.active = self.index_of(front).unwrap_or(0);
                    self.remember_tabs();
                }
            }
            TabEvent::Activated(index) => self.start_rename(index),
            TabEvent::Menu { index, at } => {
                self.show_tab(index, true);
                self.open_tab_menu(at);
            }
        }
    }

    /// The menu a right-clicked tab opens, at the pointer.
    fn open_tab_menu(&mut self, at: Point) {
        self.close_menu();
        let entries = menu::tab_entries(&self.menu_state());
        let (items, commands) = menu::rows(&entries);
        let focus = self.ui.focused();
        let style = TextStyle {
            size_px: (13.0 * self.scale + 0.5) as u16,
            ..self.small
        };
        if open_menu_at(
            &mut self.ui,
            self.strip,
            Rect::new(at.x, at.y, 1, 1),
            &items,
            style,
            Msg::Menu,
        )
        .is_some()
        {
            self.open_menu = Some(OpenMenu { commands, focus });
        }
    }

    /// Puts a field over tab `index`, holding its name.
    fn start_rename(&mut self, index: usize) {
        self.finish_rename(true);
        if index >= self.tabs.len() {
            return;
        }
        self.show_tab(index, true);
        let Some(tab) = tab_rect(&mut self.ui, self.strip, index) else {
            return;
        };
        let s = |v: i32| (v as f32 * self.scale + 0.5) as i32;
        let mut input = TextInput::<Msg>::new()
            .with_submit(Msg::Renamed)
            .with_max_chars(120)
            .with_style(self.small);
        input.set_text(self.tab_name(index));
        let rect = Rect::new(
            tab.x,
            tab.y + s(2),
            tab.width.max(s(160)),
            (tab.height - s(4)).max(s(16)),
        );
        let root = self.ui.root();
        let Some(field) = self.ui.add(root, input, rect) else {
            return;
        };
        self.ui.focus(Some(field));
        self.rename = Some(Rename {
            tab: self.tabs[index].id,
            field,
        });
    }

    /// Takes the rename field away, giving the tab what was typed when `keep`.
    /// Nothing typed, or the file's own name, gives it back its file's name.
    fn finish_rename(&mut self, keep: bool) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let text = self
            .ui
            .widget::<TextInput<Msg>>(rename.field)
            .map(|field| field.text().trim().to_string())
            .unwrap_or_default();
        self.ui.remove(rename.field);
        if keep && let Some(index) = self.index_of(rename.tab) {
            let file = self.tabs[index]
                .path
                .as_deref()
                .map_or_else(|| "Untitled".to_string(), name_of);
            self.tabs[index].name = (!text.is_empty() && text != file).then_some(text);
            self.sync_strip();
            self.remember_tabs();
        }
        if self.prompt.is_none() {
            let editor = self.editor_id();
            self.ui.focus(Some(editor));
        }
    }

    /// A click anywhere else ends a rename, keeping what was typed.
    fn settle_rename(&mut self) {
        if let Some(rename) = &self.rename
            && self.ui.focused() != Some(rename.field)
        {
            self.finish_rename(true);
        }
    }

    /// Goes to the line the tab in front is waiting for, once the index has
    /// counted that far.
    fn settle_pending_line(&mut self) {
        let Some(line) = self.tabs.get(self.active).and_then(|t| t.pending_line) else {
            return;
        };
        let reachable = {
            let doc = self.editor().document_mut();
            doc.is_indexed() || doc.known_lines() > line
        };
        if reachable {
            self.tabs[self.active].pending_line = None;
            self.editor().go_to(line);
        }
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
        let tab = &self.tabs[self.active];
        let settings = self.memory.settings.get();
        State {
            has_file: tab.path.is_some(),
            modified: self.is_modified(self.active),
            can_format: tab.format_kind.is_some(),
            read_only: tab.read_only,
            line_numbers: self.gutter,
            recent: self.memory.recent.paths().to_vec(),
            tabs: self.tabs.len(),
            tab_color: tab.color,
            reopen_tabs: settings.reopen_tabs,
            ask_before_reloading: settings.ask_before_reloading,
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

    /// Closes the open menu, and gives the keyboard back.
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
                let index = self.add_tab(FileDocument::empty());
                self.show_tab(index, true);
                self.say("new tab".into());
            }
            Command::Open => {
                if let Some(path) = self.pick_open() {
                    self.open_path(&path);
                }
            }
            Command::OpenRecent(n) => {
                if let Some(path) = self.memory.recent.paths().get(n).cloned() {
                    self.open_path(&path);
                }
            }
            Command::ClearRecent => self.memory.recent.clear(),
            Command::Close => self.close_tab(self.active),
            Command::CloseOtherTabs => self.close_other_tabs(),
            Command::Quit => {
                if self.may_discard_all() {
                    self.exit = true;
                }
            }
            Command::Save => self.save(),
            Command::SaveAs => self.save_as(),
            Command::Revert => self.reload_from_disk(true),
            Command::ReloadFromDisk => self.reload_from_disk(false),
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
                self.gutter = !self.gutter;
                let on = self.gutter;
                self.each_editor(|area| area.set_gutter(on));
            }
            Command::NextTab => self.step_tab(true),
            Command::PreviousTab => self.step_tab(false),
            Command::RenameTab => self.start_rename(self.active),
            Command::SetTabColor(color) => {
                self.tabs[self.active].color = color;
                self.sync_strip();
                self.remember_tabs();
            }
            Command::Format => self.start_format(),
            Command::ReadOnly => {
                let tab = &mut self.tabs[self.active];
                tab.read_only = !tab.read_only;
                let read_only = tab.read_only;
                self.editor().set_read_only(read_only);
                self.say(if read_only { "read only" } else { "editable" }.into());
            }
            Command::CopyPath => self.copy_path(),
            Command::Reveal => {
                let path = self.tabs[self.active].path.clone();
                if let Some(path) = path
                    && let Err(e) = reveal(&path)
                {
                    self.say(format!("showing {}: {e}", path.display()));
                }
            }
            Command::ReopenTabs => self
                .memory
                .settings
                .update(|s| s.reopen_tabs = !s.reopen_tabs),
            Command::AskBeforeReloading => self
                .memory
                .settings
                .update(|s| s.ask_before_reloading = !s.ask_before_reloading),
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

    /// Whether tab `index` may be put away: it has no unsaved changes, or the
    /// user said to save them and they are saved, or to lose them. The tab is
    /// brought to the front to be asked about.
    fn may_discard_tab(&mut self, index: usize) -> bool {
        if !self.is_modified(index) {
            return true;
        }
        self.show_tab(index, true);
        let name = self.tab_name(index);
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
        !self.is_modified(index)
    }

    /// [`may_discard_tab`](Self::may_discard_tab) for every tab, stopping at
    /// the first Cancel.
    fn may_discard_all(&mut self) -> bool {
        let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        ids.into_iter().all(|id| {
            self.index_of(id)
                .is_none_or(|index| self.may_discard_tab(index))
        })
    }

    fn pick_open(&self) -> Option<PathBuf> {
        let mut dialog = FileDialog::new().set_title("Open");
        if let Some(dir) = self.tabs[self.active]
            .path
            .as_deref()
            .and_then(Path::parent)
        {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_file()
    }

    /// Brings `path` to the front: the tab that has it, or a new one, which
    /// takes the place of an empty untitled tab in front. Whether it opened.
    fn open_path(&mut self, path: &Path) -> bool {
        if let Some(index) = self.tab_with(path) {
            self.show_tab(index, true);
            return true;
        }
        match FileDocument::open(path) {
            Ok(doc) => {
                let blank = self
                    .is_blank(self.active)
                    .then(|| self.tabs[self.active].id);
                let index = self.add_tab(doc);
                self.show_tab(index, true);
                if let Some(blank) = blank
                    && let Some(at) = self.index_of(blank)
                {
                    self.remove_tab(at);
                }
                self.memory.recent.add(path);
                self.sync_strip();
                self.remember_tabs();
                self.say(String::new());
                true
            }
            Err(e) => {
                if !path.exists() {
                    self.memory.recent.remove(path);
                }
                self.say(format!("{}: {e}", path.display()));
                false
            }
        }
    }

    /// Takes on what a tab's file decides: its tab stops, and whether it can
    /// be formatted.
    fn named(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (path, editor) = (tab.path.clone(), tab.editor);
        let tab_width = tab_width_for(path.as_deref());
        let Some(area) = self.area_mut(editor) else {
            return;
        };
        area.set_tab_width(tab_width);
        let head = area.document().head(8192);
        self.tabs[index].format_kind = format::detect(path.as_deref(), &head);
    }

    fn save(&mut self) {
        let Some(path) = self.tabs[self.active].path.clone() else {
            return self.save_as();
        };
        let result = self.editor().document_mut().save();
        match result {
            Ok(()) => {
                self.saved(&path);
                self.say("saved".into());
            }
            Err(e) => self.say(e),
        }
    }

    fn save_as(&mut self) {
        let current = self.tabs[self.active].path.clone();
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
                let index = self.active;
                self.tabs[index].path = Some(path.clone());
                self.named(index);
                self.saved(&path);
                self.memory.recent.add(&path);
                self.sync_strip();
                self.remember_tabs();
                self.say(format!("saved as {}", path.display()));
            }
            Err(e) => self.say(e),
        }
    }

    /// The file in front was just written: what is on disk is what squint has.
    fn saved(&mut self, path: &Path) {
        let tab = &mut self.tabs[self.active];
        tab.stamp = Stamp::of(path);
        tab.ignore_changes = false;
    }

    /// Reads the file in front again. With changes in it, asks first — and
    /// Revert does nothing without any.
    fn reload_from_disk(&mut self, revert: bool) {
        let index = self.active;
        if self.tabs[index].path.is_none() {
            return;
        }
        let modified = self.is_modified(index);
        if revert && !modified {
            return;
        }
        if modified && !confirm_revert(&self.tab_name(index)) {
            return;
        }
        if self.reload(index) {
            self.say(if revert { "reverted" } else { "reloaded" }.into());
        }
    }

    /// Opens tab `index`'s file again in the same tab, at the same line.
    /// Whether it opened.
    fn reload(&mut self, index: usize) -> bool {
        let Some((path, editor)) = self
            .tabs
            .get(index)
            .and_then(|t| Some((t.path.clone()?, t.editor)))
        else {
            return false;
        };
        let doc = match FileDocument::open(&path) {
            Ok(doc) => doc,
            Err(e) => {
                self.say(format!("{}: {e}", path.display()));
                return false;
            }
        };
        let head = doc.head(8192);
        let line = self.area(editor).map_or(0, |area| area.caret().line);
        self.forget_editor(editor);
        let read_only = self.tabs[index].read_only;
        if let Some(area) = self.area_mut(editor) {
            area.set_document(doc);
            area.set_read_only(read_only);
        }
        let tab = &mut self.tabs[index];
        tab.format_kind = format::detect(Some(&path), &head);
        tab.stamp = Stamp::of(&path);
        tab.ignore_changes = false;
        tab.pending_line = (line > 0).then_some(line);
        true
    }

    /// Looks at every tab's file, every couple of seconds, and deals with the
    /// ones something else has changed: a tab with no changes of its own
    /// reloads quietly unless the settings say to ask, and one with changes
    /// always asks.
    fn check_files(&mut self) {
        if self.last_check.elapsed() < CHECK_FILES || self.rename.is_some() || self.ui.popup_open()
        {
            return;
        }
        self.last_check = Instant::now();
        let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        for id in ids {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            let tab = &self.tabs[index];
            if tab.ignore_changes {
                continue;
            }
            let Some(path) = tab.path.clone() else {
                continue;
            };
            let now = Stamp::of(&path);
            if now == tab.stamp {
                continue;
            }
            self.tabs[index].stamp = now;
            let name = self.tab_name(index);
            if now.is_none() {
                self.say(format!("{name} is no longer on disk"));
                continue;
            }
            let modified = self.is_modified(index);
            if !modified && !self.memory.settings.get().ask_before_reloading {
                if self.reload(index) {
                    self.say(format!("reloaded {name}: it changed on disk"));
                }
                continue;
            }
            self.show_tab(index, true);
            match ask_about_change(&name, modified) {
                Change::Reload => {
                    if self.reload(index) {
                        self.say(format!("reloaded {name}"));
                    }
                }
                Change::Keep => self.say(format!(
                    "{name} changed on disk: Reload from Disk shows it as it is"
                )),
                Change::Ignore => {
                    self.tabs[index].ignore_changes = true;
                    self.say(format!("not watching {name} until it is reloaded"));
                }
            }
        }
    }

    fn copy_path(&mut self) {
        let Some(path) = self.tabs[self.active].path.clone() else {
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

    /// One size bigger for `1`, one smaller for `-1`, the usual for `0`. Every
    /// tab's text is the same size.
    fn zoom(&mut self, step: i32) {
        let now = self.text_size;
        let size = match step {
            0 => Some(TEXT_SIZE),
            s if s > 0 => TEXT_SIZES.iter().copied().find(|&t| t > now),
            _ => TEXT_SIZES.iter().rev().copied().find(|&t| t < now),
        }
        .unwrap_or(now);
        self.text_size = size;
        let style = self.editor_style();
        self.each_editor(|area| area.set_style(style));
        self.say(format!("text size {size}"));
    }

    fn each_editor(&mut self, mut apply: impl FnMut(&mut TextArea<Msg, FileDocument>)) {
        let editors: Vec<NodeId> = self.tabs.iter().map(|t| t.editor).collect();
        for editor in editors {
            if let Some(area) = self.area_mut(editor) {
                apply(area);
            }
        }
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
            let editor = self.editor_id();
            self.ui.focus(Some(editor));
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
        let editor = self.editor_id();
        let area = self.editor();
        let from = match area.selection() {
            Some((start, end)) => {
                if forward {
                    end
                } else {
                    start
                }
            }
            None => area.caret(),
        };
        let doc = area.document_mut();
        let Some(origin) = doc.offset_of(from) else {
            self.say("the caret's line has not been counted yet".into());
            return;
        };
        let find = doc.find(&query, origin, forward);
        self.search = Some(Search {
            editor,
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
        let editor = search.editor;
        if search.hit.is_none() {
            let deadline = Instant::now() + SLICE_TIME;
            let Some(area) = self.area_mut(editor) else {
                return;
            };
            let doc = area.document_mut();
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
        let indexed = self
            .area(editor)
            .is_some_and(|area| area.document().is_indexed());
        if !indexed {
            self.say("found — counting lines to reach it…".into());
            self.search = Some(search);
            return;
        }
        let end = at + search.query.len() as u64;
        let Some(area) = self.area_mut(editor) else {
            return;
        };
        let (from, to) = {
            let doc = area.document_mut();
            (doc.pos_of(at), doc.pos_of(end))
        };
        match (from, to) {
            (Some(from), Some(to)) => {
                area.select_range(from, to);
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
    /// frame, and opens that in a tab of its own when it is complete.
    fn start_format(&mut self) {
        if self.formatting.is_some() {
            return;
        }
        let editor = self.editor_id();
        let path = self.tabs[self.active].path.clone();
        let head = self.editor_ref().document().head(8192);
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
            editor,
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
            let Some(area) = self.area(running.editor) else {
                let _ = fs::remove_file(&running.part);
                return;
            };
            let doc = area.document();
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

    /// Puts the finished file in place and opens it in a new tab.
    fn finish_format(&mut self, running: Formatting) {
        let Formatting {
            job,
            note,
            writer,
            part,
            out,
            ..
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
        if let Some(index) = self.tab_with(&out) {
            // Formatted before: the tab showing the old result reads the new.
            self.reload(index);
            self.show_tab(index, true);
        } else {
            match FileDocument::open(&out) {
                Ok(doc) => {
                    let index = self.add_tab(doc);
                    self.show_tab(index, true);
                    self.remember_tabs();
                }
                Err(e) => {
                    self.say(format!("{}: {e}", out.display()));
                    return;
                }
            }
        }
        let kind = job.kind().name();
        self.say(format!(
            "formatted as {kind} with {note} into {}",
            out.display()
        ));
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
        let name = self.tab_name(self.active);
        let editor = self.editor();
        let caret = editor.caret();
        let modified = editor.document().is_modified();
        let doc = editor.document_mut();
        let error = doc.take_error();
        let (scanned, total) = doc.index_progress();
        let lines = if doc.is_indexed() {
            format!("{} lines", doc.line_count().unwrap_or(0))
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
        self.title = format!("{}{name} — squint", if modified { "• " } else { "" });
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            // What a search or a format has read moved with the edit.
            Msg::Changed => {
                let editor = self.editor_id();
                if self.search.as_ref().is_some_and(|s| s.editor == editor) {
                    self.search = None;
                }
                if self.formatting.as_ref().is_some_and(|f| f.editor == editor) {
                    self.cancel_format("the text changed");
                }
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
            Msg::Tab(event) => self.tab_event(event),
            Msg::Renamed => self.finish_rename(true),
        }
    }

    /// Marks every match of what is typed in the find field on the lines on
    /// screen of the tab in front, as it is typed, and nothing once the field
    /// closes — or in a tab that has gone behind.
    fn sync_highlight(&mut self) {
        let query = match self.prompt.as_ref().map(|p| p.ask) {
            Some(Ask::Find) => self.prompt_text(),
            _ => String::new(),
        };
        let editor = self.editor_id();
        if self
            .highlighted
            .as_ref()
            .is_some_and(|(node, marked)| *node == editor && *marked == query)
        {
            return;
        }
        if let Some((node, _)) = self.highlighted.take()
            && node != editor
            && let Some(area) = self.area_mut(node)
        {
            area.document_mut().set_highlight("");
        }
        if let Some(area) = self.area_mut(editor) {
            area.document_mut().set_highlight(&query);
        }
        self.highlighted = Some((editor, query));
    }

    /// Whether the find field has the keyboard.
    fn typing_a_find(&self) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|p| p.ask == Ask::Find && self.ui.focused() == Some(p.field))
    }

    /// Whether any tab has a file to keep an eye on.
    fn watching(&self) -> bool {
        self.tabs
            .iter()
            .any(|t| t.path.is_some() && !t.ignore_changes)
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

/// Tab stops as the file's project sets them, or every four columns.
fn tab_width_for(path: Option<&Path>) -> u8 {
    path.map(editorconfig::properties_for)
        .and_then(|props| props.tab_width())
        .unwrap_or(4)
}

/// Asks whether to throw the changes to `name` away.
fn confirm_revert(name: &str) -> bool {
    let answer = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("squint")
        .set_description(format!(
            "Revert {name} to the saved version?\n\nYour changes will be lost."
        ))
        .set_buttons(MessageButtons::OkCancelCustom(
            "Revert".into(),
            "Cancel".into(),
        ))
        .show();
    match answer {
        MessageDialogResult::Ok => true,
        MessageDialogResult::Custom(button) => button == "Revert",
        _ => false,
    }
}

/// Asks what to do about `name`, which something else has changed.
fn ask_about_change(name: &str, modified: bool) -> Change {
    let consequence = if modified {
        "Reloading it throws away the changes you have made to it here."
    } else {
        "What squint shows may not match it until it is reloaded."
    };
    let answer = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("squint")
        .set_description(format!(
            "{name} has been changed by another program.\n\n{consequence}"
        ))
        .set_buttons(MessageButtons::YesNoCancelCustom(
            "Reload".into(),
            "Keep".into(),
            "Ignore Further Changes".into(),
        ))
        .show();
    match answer {
        MessageDialogResult::Yes => Change::Reload,
        MessageDialogResult::Custom(button) if button == "Reload" => Change::Reload,
        MessageDialogResult::Custom(button) if button == "Ignore Further Changes" => Change::Ignore,
        _ => Change::Keep,
    }
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
            // Ctrl let go ends a walk along the tabs. A key that arrives
            // without Ctrl says so too, in case the release itself went
            // somewhere else — to another application, say.
            if let InputEvent::Key {
                code,
                state,
                modifiers,
                ..
            } = event
                && self.switcher.walking()
                && (!modifiers.contains(Modifiers::CTRL)
                    || (*state == ElementState::Up
                        && matches!(code, KeyCode::ControlLeft | KeyCode::ControlRight)))
            {
                self.ctrl_released();
            }
            match event {
                // Answered by `close_requested`, which could still say no.
                InputEvent::CloseRequested => continue,
                InputEvent::Key {
                    code: KeyCode::Escape,
                    state: ElementState::Down,
                    ..
                } if self.rename.is_some() && !menu_up => {
                    self.finish_rename(false);
                    continue;
                }
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
                // Ctrl+Tab, on every platform: the control key, not ⌘.
                InputEvent::Key {
                    code: KeyCode::Tab,
                    state: ElementState::Down,
                    modifiers,
                    ..
                } if modifiers.contains(Modifiers::CTRL) && !menu_up => {
                    self.cycle_tab(modifiers.contains(Modifiers::SHIFT));
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
        self.settle_rename();
        self.sync_highlight();
        self.pump_index();
        self.settle_pending_line();
        self.pump_find();
        self.pump_format();
        self.pump_syntax();
        self.check_files();
        self.sync_strip();
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

    /// The close button asks about every tab's unsaved changes first, and a
    /// Cancel keeps the window.
    fn close_requested(&mut self) -> bool {
        self.may_discard_all()
    }

    /// Every way out comes through here, ⌘Q and the Dock's Quit included:
    /// the place the tabs are sure to be written down.
    fn exiting(&mut self) {
        self.remember_tabs();
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
        let animation = self
            .ui
            .next_wake_ms()
            .map(|w| Duration::from_millis(w.saturating_sub(now)));
        // And when the files are next due a look.
        let files = self
            .watching()
            .then(|| CHECK_FILES.saturating_sub(self.last_check.elapsed()));
        match (animation, files) {
            (Some(a), Some(f)) => Some(a.min(f)),
            (a, f) => a.or(f),
        }
    }
}
